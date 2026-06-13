pub mod base;
pub mod full_range;
pub mod mev_resist;
pub mod oracle;
pub mod twamm;

use std::collections::{HashMap, HashSet};

use evm_ekubo_sdk::{
    math::uint::U256,
    quoting::types::{NodeKey, TokenAmount},
};
use tycho_common::{
    simulation::errors::{SimulationError, TransitionError},
    Bytes,
};

use super::state::EkuboState;

/// Serializes an `Arc`-wrapped SDK pool as its inner value, so wrapping `imp` in an `Arc` (to make
/// the per-quote `new_state` clone a refcount bump instead of a tick-`Vec` copy) leaves the
/// serialized form identical to the pre-`Arc` representation.
pub(super) mod arc_imp {
    use std::sync::Arc;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(crate) fn serialize<T: Serialize, S: Serializer>(
        imp: &Arc<T>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        T::serialize(imp.as_ref(), serializer)
    }

    pub(crate) fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Arc<T>, D::Error> {
        T::deserialize(deserializer).map(Arc::new)
    }
}

pub struct EkuboPoolQuote {
    pub consumed_amount: i128,
    pub calculated_amount: u128,
    pub gas: u64,
    pub new_state: EkuboState,
}

#[enum_delegate::register]
pub trait EkuboPool {
    fn key(&self) -> &NodeKey;
    fn sqrt_ratio(&self) -> U256;

    fn set_sqrt_ratio(&mut self, sqrt_ratio: U256);
    fn set_liquidity(&mut self, liquidity: u128);

    fn finish_transition(
        &mut self,
        updated_attributes: HashMap<String, Bytes>,
        deleted_attributes: HashSet<String>,
    ) -> Result<(), TransitionError>;

    fn quote(
        &self,
        token_amount: TokenAmount,
    ) -> Result<super::pool::EkuboPoolQuote, SimulationError>;
    fn get_limit(&self, token_in: U256) -> Result<i128, SimulationError>;
}
