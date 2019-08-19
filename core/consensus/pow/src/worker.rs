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
    std::{
        fmt::Debug,
        marker::PhantomData,
        sync::{Arc, RwLock},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    },
    futures::{
        future::{self, Either, Loop},
        Future, IntoFuture,
    },
    log::{warn, info},
    tokio::timer::Delay,
};
use {
    client::{
        ChainHead,
        blockchain::HeaderBackend,
    },
    consensus_common::{
        Environment, Proposer, SyncOracle, ImportBlock,
        BlockImport, BlockOrigin, ForkChoiceStrategy,
    },
    inherents::InherentDataProviders,
    primitives::Pair,
    runtime_primitives::{
        codec::{Decode, Encode},
        generic::BlockId,
        traits::{
            Block, Header,
            Digest, DigestFor, DigestItemFor, NumberFor,
            ProvideRuntimeApi,
            As, SimpleArithmetic, Zero, One,
        },
    },
};
use {
    pow_primitives::{YeePOWApi, DifficultyType},
};
use super::{
    CompatibleDigestItem, WorkProof, ProofNonce,
    pow::PowSeal,
};

pub trait PowWorker<B: Block> {
    type Error: Debug + Send;
    type OnTemplate: IntoFuture<Item=B, Error=Self::Error>;
    type OnJob: IntoFuture<Item=(), Error=Self::Error>;

    fn stop_sign(&self) -> Arc<RwLock<bool>>;

    fn on_start(&self) -> Result<(), Self::Error>;

    fn on_template(&self) -> Self::OnTemplate;

    fn on_job(&self, iter: u64) -> Self::OnJob;
}

pub struct DefaultWorker<B, P, C, I, E, AccountId, SO> {
    authority_key: Arc<P>,
    client: Arc<C>,
    block_import: Arc<I>,
    env: Arc<E>,
    sync_oracle: SO,
    inherent_data_providers: InherentDataProviders,
    coin_base: AccountId,
    stop_sign: Arc<RwLock<bool>>,
    phantom: PhantomData<(B, AccountId)>,
}

impl<B, P, C, I, E, AccountId, SO> DefaultWorker<B, P, C, I, E, AccountId, SO> where
    B: Block,
    E: Environment<B> + 'static,
    <E as Environment<B>>::Error: Debug + Send,
{
    pub fn new(
        authority_key: Arc<P>,
        client: Arc<C>,
        block_import: Arc<I>,
        env: Arc<E>,
        sync_oracle: SO,
        inherent_data_providers: InherentDataProviders,
        coin_base: AccountId,
        phantom: PhantomData<(B, AccountId)>,
    ) -> Self {
        DefaultWorker {
            authority_key,
            client,
            block_import,
            env,
            sync_oracle,
            inherent_data_providers,
            coin_base,
            stop_sign: Default::default(),
            phantom,
        }
    }
}

impl<B, P, C, I, E, AccountId, SO> PowWorker<B> for DefaultWorker<B, P, C, I, E, AccountId, SO> where
    B: Block,
    DigestFor<B>: Digest,
    P: Pair,
    <P as Pair>::Public: Clone + Debug + Decode + Encode + Send + 'static,
    C: ChainHead<B> + HeaderBackend<B> + ProvideRuntimeApi + 'static,
    <C as ProvideRuntimeApi>::Api: YeePOWApi<B>,
    I: BlockImport<B, Error=consensus_common::Error> + Send + Sync + 'static,
    E: Environment<B> + 'static,
    <E as Environment<B>>::Proposer: Proposer<B>,
    <E as Environment<B>>::Error: Debug,
    <<E as Environment<B>>::Proposer as Proposer<B>>::Create: IntoFuture<Item=B>,
    <<<E as Environment<B>>::Proposer as Proposer<B>>::Create as IntoFuture>::Future: Send + 'static,
    AccountId: Clone + Debug + Decode + Encode + Default + Send + 'static,
    SO: SyncOracle + Send + Clone,
    DigestItemFor<B>: CompatibleDigestItem<B, P::Public>,
{
    type Error = consensus_common::Error;
    type OnTemplate = Box<dyn Future<Item=B, Error=Self::Error> + Send>;
    type OnJob = Box<dyn Future<Item=(), Error=Self::Error> + Send>;

    fn stop_sign(&self) -> Arc<RwLock<bool>> {
        self.stop_sign.clone()
    }

    fn on_start(&self) -> Result<(), consensus_common::Error> {
        super::register_inherent_data_provider(&self.inherent_data_providers)
    }

    fn on_template(&self) -> Self::OnTemplate {
        let get_data = || {
            let chain_head = self.client.best_block_header()
                .map_err(to_common_error)?;
            let inherent_data = self.inherent_data_providers.create_inherent_data()
                .map_err(to_common_error)?;
            Ok((chain_head, inherent_data))
        };
        let (chain_head, inherent_data) = match get_data() {
            Ok((p, data)) => (p, data),
            Err(e) => {
                warn!("failed to get proposer {:?}", e);
                return Box::new(future::err(e));
            }
        };
        let proposer = match self.env.init(&chain_head, &vec![]) {
            Ok(p) => p,
            Err(e) => {
                return Box::new(future::err(to_common_error(e)));
            }
        };

        Box::new(
            proposer.propose(inherent_data, Duration::from_secs(10))
                .into_future()
                .map_err(to_common_error)
        )
    }

    fn on_job(&self,
              iter: u64,
    ) -> Self::OnJob {
        let client = self.client.clone();
        let block_import = self.block_import.clone();
        let authority_id = self.authority_key.public();

        let proposal_work = self.on_template().into_future();

        // let coin_base = self.coin_base.clone();
        let on_proposal_block = move |block: B| -> Result<(), consensus_common::Error> {
            let (header, body) = block.deconstruct();
            let header_num = header.number().clone();
            let header_pre_hash = header.hash();
            let timestamp = timestamp_now()?;
            let difficulty = calc_difficulty(client, &header, timestamp)?;
            info!("block template {} @ {:?} difficulty {:#x}", header_num, header_pre_hash, difficulty);

            // TODO: remove hardcoded
            const PREFIX: &str = "yeeroot-";

            for i in 0_u64..iter {
                let mut work_header = header.clone();
                let proof = WorkProof::Nonce(ProofNonce::get_with_prefix_len(PREFIX, 12, i));
                let seal = PowSeal {
                    authority_id: authority_id.clone(),
                    difficulty,
                    timestamp,
                    work_proof: proof,
                };
                let item = <DigestItemFor<B> as CompatibleDigestItem<B, P::Public>>::pow_seal(seal.clone());
                work_header.digest_mut().push(item);

                let post_hash = work_header.hash();
                if let Ok(_) = seal.check_seal(post_hash, header_pre_hash) {
                    let valid_seal = work_header.digest_mut().pop().expect("must exists");
                    let import_block: ImportBlock<B> = ImportBlock {
                        origin: BlockOrigin::Own,
                        header,
                        justification: None,
                        post_digests: vec![valid_seal],
                        body: Some(body),
                        finalized: false,
                        auxiliary: Vec::new(),
                        fork_choice: ForkChoiceStrategy::LongestChain,
                    };
                    block_import.import_block(import_block, Default::default())?;

                    info!("block mined @ {} {:?}", header_num, post_hash);
                    return Ok(());
                }
            }

            Ok(())
        };

        Box::new(
            proposal_work
                .map_err(to_common_error)
                .map(|block| {
                    if let Err(e) = on_proposal_block(block) {
                        warn!("block proposal failed {:?}", e);
                    }
                })
        )
    }
}

fn calc_difficulty<B, C, AccountId>(
    client: Arc<C>, header: &<B as Block>::Header, timestamp: u64,
) -> Result<DifficultyType, consensus_common::Error> where
    B: Block,
    NumberFor<B>: SimpleArithmetic,
    DigestFor<B>: Digest,
    DigestItemFor<B>: super::CompatibleDigestItem<B, AccountId>,
    C: HeaderBackend<B> + ProvideRuntimeApi,
    <C as ProvideRuntimeApi>::Api: YeePOWApi<B>,
    AccountId: Encode + Decode + Debug,
{
    let curr_block_id = BlockId::hash(*header.parent_hash());
    let api = client.runtime_api();
    let genesis_difficulty = api.genesis_difficulty(&curr_block_id)
        .map_err(to_common_error)?;
    let adj = api.difficulty_adj(&curr_block_id)
        .map_err(to_common_error)?;
    let curr_header = client.header(curr_block_id)
        .expect("parent block must exist for sealer; qed")
        .expect("parent block must exist for sealer; qed");

    // not on adjustment, reuse parent difficulty
    if *header.number() % adj != Zero::zero() {
        let curr_difficulty = curr_header.digest().logs().iter().rev()
            .filter_map(CompatibleDigestItem::as_pow_seal).next()
            .and_then(|seal| Some(seal.difficulty))
            .unwrap_or(genesis_difficulty);
        return Ok(curr_difficulty);
    }

    let mut curr_header = curr_header;
    let mut curr_seal = curr_header.digest().logs().iter().rev()
        .filter_map(CompatibleDigestItem::as_pow_seal).next()
        .expect("Seal must exist when adjustment comes; qed");
    let curr_difficulty = curr_seal.difficulty;
    let (last_num, last_time) = loop {
        let prev_header = client.header(BlockId::hash(*curr_header.parent_hash()))
            .expect("parent block must exist for sealer; qed")
            .expect("parent block must exist for sealer; qed");
        assert!(*prev_header.number() + One::one() == *curr_header.number());
        let prev_seal = prev_header.digest().logs().iter().rev()
            .filter_map(CompatibleDigestItem::as_pow_seal).next();
        if *prev_header.number() % adj == Zero::zero() {
            break (curr_header.number(), curr_seal.timestamp);
        }
        if let Some(prev_seal) = prev_seal {
            curr_header = prev_header;
            curr_seal = prev_seal;
        } else {
            break (curr_header.number(), curr_seal.timestamp);
        }
    };

    let target_block_time = api.target_block_time(&curr_block_id)
        .map_err(to_common_error)?;
    let block_gap = As::<u64>::as_(*header.number() - *last_num);
    let time_gap = timestamp - last_time;
    let expected_gap = target_block_time * 1000 * block_gap;
    let new_difficulty = (curr_difficulty / expected_gap) * time_gap;
    info!("difficulty adjustment: gap {} time {}", block_gap, time_gap);
    info!("    new difficulty {:#x}", new_difficulty);

    Ok(new_difficulty)
}

fn timestamp_now() -> Result<u64, consensus_common::Error> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)
        .map_err(to_common_error)?.as_millis() as u64)
}

fn to_common_error<E: Debug>(e: E) -> consensus_common::Error {
    consensus_common::ErrorKind::ClientImport(format!("{:?}", e)).into()
}

pub fn start_worker<B, C, I, W, SO, OnExit>(
    client: Arc<C>,
    worker: Arc<W>,
    sync_oracle: SO,
    on_exit: OnExit,
) -> Result<impl Future<Item=(), Error=()>, consensus_common::Error> where
    B: Block,
    C: ChainHead<B>,
    I: BlockImport<B>,
    W: PowWorker<B>,
    SO: SyncOracle,
    OnExit: Future<Item=(), Error=()>,
{
    worker.on_start().map_err(to_common_error)?;

    let stop_sign = worker.stop_sign();

    info!("worker loop start");
    let work = future::loop_fn((), move |()| {
        let delay = Delay::new(Instant::now() + Duration::new(5, 0));
        let delayed_continue = Either::A(delay.then(|_| future::ok(Loop::Continue(()))));
        let no_delay_stop = Either::B(future::ok(Loop::Break(())));

        match worker.stop_sign().read() {
            Ok(stop_sign) => {
                if *stop_sign {
                    return Either::A(no_delay_stop);
                }
            }
            Err(e) => {
                warn!("work stop sign read error {:?}", e);
                return Either::A(no_delay_stop);
            }
        }

        // worker main loop
        info!("worker one loop start");

        if sync_oracle.is_major_syncing() {
            return Either::A(delayed_continue);
        }

        let task = worker.on_job(10000).into_future();
        Either::B(
            task.then(|_| Delay::new(Instant::now()))
                .then(|_| Ok(Loop::Continue(())))
        )
    });

    Ok(work.select(on_exit).then(move |_| {
        stop_sign.write()
            .map(|mut sign| { *sign = true; })
            .unwrap_or_else(|e| { warn!("write stop sign error : {:?}", e); });

        Ok(())
    }))
}
