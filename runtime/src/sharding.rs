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
    support::{
        decl_module, decl_storage,
    },
};

pub trait Trait: system::Trait {
    //
}

decl_storage! {
    trait Store for Module<T: Trait> as Sharding {
        /// Shard number for this chain
        /// may be not configured, and generated in generated block One
        pub CurrentShard get(current_shard): Option<u32>;

        /// Total sharding count, configured from genesis block
        pub ShardingCount get(sharding_count) config(): u32;
    }
}

decl_module! {
    pub struct Module<T: Trait> for enum Call where origin: T::Origin { }
}

impl<T: Trait> Module<T> {
    //
}
