// Copyright (C) 2019 Yee Foundation.
//
// This file is part of YeeChain.
//
// YeeChain is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// YeeChain is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with YeeChain.  If not, see <https://www.gnu.org/licenses/>.

use {
    structopt::StructOpt,
    substrate_cli::{impl_augment_clap},
};
use log::{info, warn};
use substrate_service::{FactoryFullConfiguration, ServiceFactory, config::Roles, new_client, FactoryBlock, FullClient};
use substrate_client::ChainHead;
use runtime_primitives::{
    generic::BlockId,
    traits::{ProvideRuntimeApi, Block, Header, Digest as DigestT, DigestItemFor},
};
use crate::error;
use crate::service::{NodeConfig};
use crate::cli::FactoryBlockNumber;
use yee_bootnodes_router;
use yee_bootnodes_router::BootnodesRouterConf;
use yee_runtime::AccountId;
use yee_primitives::{AddressCodec, Address, Hrp};
use yee_sharding::{ShardingDigestItem, ScaleOutPhase, ScaleOutPhaseDigestItem};
use sharding_primitives::{ShardingAPI, ScaleOut, utils::shard_num_for};
use inherents::{
    InherentDataProviders, RuntimeString,
};

#[derive(Clone, Debug, Default, StructOpt)]
pub struct YeeCliConfig {
    /// Specify miner coinbase for block authoring
    #[structopt(long = "coinbase", value_name = "COINBASE")]
    pub coinbase: Option<String>,

    /// Specify shard number
    #[structopt(long = "shard-num", value_name = "SHARD_NUM")]
    pub shard_num: u16,

    /// Specify a list of bootnodes-routers
    #[structopt(long = "bootnodes-routers", value_name = "URL")]
    pub bootnodes_routers: Vec<String>,

    /// Specify foreign p2p protocol TCP port
    #[structopt(long = "foreign-port", value_name = "PORT")]
    pub foreign_port: Option<u16>,

    /// Whether use dev params or not
    #[structopt(long = "dev-params")]
    pub dev_params: bool,

    /// Specify params number
    #[structopt(long = "dev-params-num")]
    pub dev_params_num: Option<u16>,

    /// Whether mine
    #[structopt(long = "mine")]
    pub mine: bool,
}

impl_augment_clap!(YeeCliConfig);

pub fn process_custom_args<F, C>(config: &mut FactoryFullConfiguration<F>, custom_args: &YeeCliConfig) -> error::Result<()>
where
    F: ServiceFactory<Configuration=NodeConfig<F>>,
    DigestItemFor<FactoryBlock<F>>: ShardingDigestItem<u16> + ScaleOutPhaseDigestItem<FactoryBlockNumber<F>, u16>,
    FullClient<F>: ProvideRuntimeApi + ChainHead<FactoryBlock<F>>,
    <FullClient<F> as ProvideRuntimeApi>::Api: ShardingAPI<FactoryBlock<F>>,
{

    let (shard_num, shard_count, scale_out) = get_shard_info::<F>(&config, &custom_args)?;

    config.custom.hrp = get_hrp( config.chain_spec.id());

    config.custom.shard_num = shard_num;
    config.custom.shard_count = shard_count;
    config.custom.scale_out = scale_out.clone();

    if config.roles == Roles::AUTHORITY{
        let coinbase = custom_args.coinbase.clone().ok_or(error::ErrorKind::Input("Coinbase not found".to_string()))?;
        let (coinbase, hrp) = parse_coinbase(coinbase).map_err(|e| format!("Invalid coinbase: {:?}", e))?;

        if config.custom.hrp != hrp{
            return Err(error::ErrorKind::Input("Invalid coinbase hrp".to_string()).into());
        }

        let coinbase_shard_num = shard_num_for(&coinbase, shard_count).expect("qed");
        info!("Coinbase shard num: {}", coinbase_shard_num);
        if coinbase_shard_num != shard_num {
            return Err(error::ErrorKind::Input("Invalid coinbase: shard num is not accordant".to_string()).into());
        }

        //check if coinbase is accordant after scaling out
        if let Some(ScaleOut{shard_num: scale_out_shard_num}) = scale_out {
            let scale_out_shard_count = shard_count * 2;
            let coinbase_shard_num = shard_num_for(&coinbase, scale_out_shard_count).expect("qed");
            info!("Coinbase shard num after scaling out: {}", coinbase_shard_num);
            if coinbase_shard_num != scale_out_shard_num {
                return Err(error::ErrorKind::Input("Invalid coinbase: shard num is not accordant after scaling out".to_string()).into());
            }
        }

        config.custom.coinbase = coinbase;
    }

    let bootnodes_routers = custom_args.bootnodes_routers.clone();

    if bootnodes_routers.len() > 0{

        match get_bootnodes_router_conf(&bootnodes_routers){
            Ok(bootnodes_router_conf) => {

                match get_native_bootnodes(&bootnodes_router_conf, config.custom.shard_num){
                    Ok(bootnodes) => {
                        config.network.boot_nodes = bootnodes;
                    },
                    Err(_e) => {
                        warn!("Failed to get bootnodes: {:?}", bootnodes_routers);
                    }
                }

                config.custom.bootnodes_router_conf = Some(bootnodes_router_conf);
            },
            Err(_) => {
                warn!("Failed to get bootnodes router conf: {:?}", bootnodes_routers);
            }
        }
    }

    config.custom.foreign_port = custom_args.foreign_port;
    config.custom.mine = custom_args.mine;

    info!("Custom params: ");
    info!("  coinbase: {}", config.custom.coinbase.to_address(config.custom.hrp.clone()).expect("qed"));
    info!("  shard num: {}", config.custom.shard_num);
    info!("  shard count: {}", config.custom.shard_count);
    info!("  bootnodes: {:?}", config.network.boot_nodes);
    info!("  foreign port: {:?}", config.custom.foreign_port);
    info!("  bootnodes router conf: {:?}", config.custom.bootnodes_router_conf);
    info!("  mine: {:?}", config.custom.mine);

    register_inherent_data_provider(&config.custom.inherent_data_providers, shard_num, shard_count, scale_out)
        .map_err(|e| format!("Inherent data error: {:?}", e))?;

    Ok(())
}

fn get_bootnodes_router_conf(bootnodes_routers :&Vec<String>) -> error::Result<BootnodesRouterConf>{

    yee_bootnodes_router::client::call(|mut client|{
        let result = client.bootnodes().call().map_err(|e|format!("{:?}", e))?;
        Ok(result)
    }, bootnodes_routers).map_err(|e|format!("{:?}", e).into())

}

fn get_native_bootnodes(bootnodes_router_conf: &BootnodesRouterConf, shard_num: u16) -> error::Result<Vec<String>>{

    match bootnodes_router_conf.shards.get(format!("{}",shard_num).as_str()){
        Some(result) => Ok(result.native.clone()),
        None => Err(error::ErrorKind::Msg("Not found shard in bootnodes_router_conf".to_string()).into())
    }
}

fn parse_coinbase(input: String) -> error::Result<(AccountId, Hrp)> {

    let address = Address(input);
    let (coinbase, hrp) = AccountId::from_address(&address).map_err(|e| format!("{:?}", e))?;

    Ok((coinbase, hrp))
}

fn get_hrp(chain_spec_id: &str) -> Hrp{

    match chain_spec_id{
        "main" => Hrp::MAINNET,
        _ => Hrp::TESTNET,
    }
}

fn get_shard_info<F>(config: &FactoryFullConfiguration<F>, custom_args: &YeeCliConfig) -> error::Result<(u16, u16, Option<ScaleOut>)>
where
    F: ServiceFactory<Configuration=NodeConfig<F>>,
    DigestItemFor<FactoryBlock<F>>: ShardingDigestItem<u16> + ScaleOutPhaseDigestItem<FactoryBlockNumber<F>, u16>,
    FullClient<F>: ProvideRuntimeApi + ChainHead<FactoryBlock<F>>,
    <FullClient<F> as ProvideRuntimeApi>::Api: ShardingAPI<FactoryBlock<F>>,
{
    let client = new_client::<F>(&config)?;
    let last_block_header = client.best_block_header()?;

    let shard_info : Option<(u16, u16)> = last_block_header.digest().logs().iter().rev()
        .filter_map(ShardingDigestItem::as_sharding_info)
        .next();

    if let Some((num, cnt)) = shard_info {
        info!("Original shard info: shard num: {}, shard count: {}", num, cnt);
    }

    let arg_shard_num = custom_args.shard_num;

    // check if there is scale out phase log in header
    // we treat that there is commiting scale out phase in genesis block header
    let scale_out_phase = match shard_info {
        Some(shard_info) => {
            last_block_header.digest().logs().iter().rev()
                .filter_map(ScaleOutPhaseDigestItem::<FactoryBlockNumber<F>, u16>::as_scale_out_phase)
                .next()
        },
        None => {
            // genesis block
            let api = client.runtime_api();
            let last_block_id = BlockId::hash(last_block_header.hash());
            let genesis_shard_cnt = api.get_genesis_shard_count(&last_block_id)?;

            Some(ScaleOutPhase::<FactoryBlockNumber<F>, _>::Commiting {
                shard_count: genesis_shard_cnt,
            })
        },
    };

    let ret_shard_info =  if let Some(ScaleOutPhase::Commiting {shard_count}) = scale_out_phase {
        // scale out phase commiting
        match shard_info{
            Some((ori_shard_num, ori_shard_count)) => {
                //not genesis block
                if arg_shard_num == ori_shard_num || arg_shard_num == ori_shard_num + ori_shard_count {
                    (arg_shard_num, shard_count, None)
                }else{
                    return Err(error::ErrorKind::Input(
                        format!("Invalid shard num on non-genesis block scale out phase commiting")).into());
                }
            },
            None => {
                //genesis block
                if arg_shard_num > shard_count{
                    return Err(error::ErrorKind::Input(format!("Invalid shard num on genesis block scale out phase commiting")).into());
                }
                (arg_shard_num, shard_count, None)
            }
        }
    }else{
        // normal
        let (ori_shard_num, ori_shard_count) = shard_info.expect("qed");
        if arg_shard_num == ori_shard_num {//normal
            (ori_shard_num, ori_shard_count, None)
        }else if arg_shard_num == ori_shard_num + ori_shard_count {//scale out
            (ori_shard_num, ori_shard_count, Some(ScaleOut{shard_num: arg_shard_num}))
        }else{
            return Err(error::ErrorKind::Input(format!("Invalid shard num")).into());
        }
    };

    Ok(ret_shard_info)
}

fn register_inherent_data_provider(
    inherent_data_providers: &InherentDataProviders,
    shard_num: u16,
    shard_cnt: u16,
    scale_out: Option<ScaleOut>,
) -> Result<(), consensus_common::Error> where
{
    if !inherent_data_providers.has_provider(&srml_sharding::INHERENT_IDENTIFIER) {
        inherent_data_providers.register_provider(srml_sharding::InherentDataProvider::new(
            shard_num,
            shard_cnt,
            scale_out.map(|x|srml_sharding::ScaleOut{shard_num: x.shard_num})
        )).map_err(inherent_to_common_error)
    } else {
        Ok(())
    }
}

fn inherent_to_common_error(err: RuntimeString) -> consensus_common::Error {
    consensus_common::ErrorKind::InherentData(err.into()).into()
}
