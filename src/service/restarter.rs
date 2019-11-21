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

use substrate_service::{ServiceFactory, TaskExecutor, Arc, FactoryBlock, ComponentClient, Components};
use futures::Stream;
use substrate_client::{BlockchainEvents};
use yee_sharding::{ShardingDigestItem, ScaleOutPhaseDigestItem, ScaleOutPhase};
use runtime_primitives::traits::{Block as BlockT, Header as HeaderT, Digest as DigestT, DigestItemFor};
use parking_lot::RwLock;
use crate::cli::{CliTriggerExit, CliSignal, FactoryBlockNumber};
use log::info;
use substrate_cli::{TriggerExit};
use yee_runtime::AccountId;
use sharding_primitives::utils::shard_num_for;
use crate::service::ScaleOut;
use std::thread::sleep;
use std::time::Duration;

pub struct Params {
	pub coinbase: AccountId,
	pub shard_num: u16,
	pub shard_count: u16,
	pub scale_out: Option<ScaleOut>,
	pub trigger_exit: Option<Arc<CliTriggerExit<CliSignal>>>,
}

pub fn start_restarter<C>(param: Params, client: Arc<ComponentClient<C>>, executor: &TaskExecutor) where
	C: Components,
	<<C::Factory as ServiceFactory>::Block as BlockT>::Header: HeaderT,
	DigestItemFor<<C::Factory as ServiceFactory>::Block>: ScaleOutPhaseDigestItem<FactoryBlockNumber<C::Factory>, u16>,
{
	let target_shard_num = match param.scale_out.clone(){
		Some(scale_out) => scale_out.shard_num,
		None => param.shard_num,
	};

	let coinbase = param.coinbase.clone();
	let trigger_exit = param.trigger_exit.expect("qed");

	let task = client.import_notification_stream().for_each(move |notification| {

		let header = notification.header;

		let scale_out_phase : Option<ScaleOutPhase<FactoryBlockNumber<C::Factory>, u16>> = header.digest().logs().iter().rev()
			.filter_map(ScaleOutPhaseDigestItem::as_scale_out_phase)
			.next();

		if let Some(scale_out_phase) = scale_out_phase.clone(){
			info!("Scale out phase: {:?}", scale_out_phase);
		}

		if let Some(ScaleOutPhase::Commiting{shard_count}) = scale_out_phase{

			let coinbase_shard_num = shard_num_for(&coinbase, shard_count).expect("qed");
			if target_shard_num != coinbase_shard_num {
				info!("Stop service for coinbase shard num is not accordant");
				sleep(Duration::from_secs(1));
				trigger_exit.trigger_exit(CliSignal::Stop);
				return Ok(());
			}

			info!("Restart service for commiting scale out phase");
			sleep(Duration::from_secs(1));
			trigger_exit.trigger_exit(CliSignal::Restart);

		}

		Ok(())
	});

	executor.spawn(task);

}
