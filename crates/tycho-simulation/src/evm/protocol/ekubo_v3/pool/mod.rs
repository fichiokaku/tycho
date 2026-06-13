pub mod boosted_fees;
pub mod concentrated;
pub mod full_range;
pub mod mev_capture;
pub mod oracle;
pub mod stableswap;
mod timed;
pub mod twamm;

use std::collections::{HashMap, HashSet};

use ekubo_sdk::{
    chain::evm::{EvmPoolKey, EvmTokenAmount},
    U256,
};
use revm::primitives::Address;
use tycho_common::{
    simulation::errors::{SimulationError, TransitionError},
    Bytes,
};

use super::state::EkuboV3State;

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
    pub new_state: EkuboV3State,
}

#[enum_delegate::register]
pub trait EkuboPool {
    fn key(&self) -> EvmPoolKey;
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
        token_amount: EvmTokenAmount,
    ) -> Result<super::pool::EkuboPoolQuote, SimulationError>;
    fn get_limit(&self, token_in: Address) -> Result<i128, SimulationError>;
}
