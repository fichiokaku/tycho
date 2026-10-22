//! Biconomy PropAMM venue: one push-payment `IPropAMM` contract serving many pairs, quoting the
//! boards makers store in the PropAMM executor. The state is the executor's raw board storage,
//! priced at the execution block the way `PropAMMVenue.quote` prices it.

use tycho_client::feed::BlockHeader;

use crate::evm::decoder::TychoStreamDecoder;

mod decoder;
mod math;
pub mod state;

pub use state::BiconomyState;

pub const PROTOCOL_SYSTEM: &str = "biconomy";

pub fn register_biconomy_decoder(decoder: &mut TychoStreamDecoder<BlockHeader>) {
    decoder.register_decoder::<BiconomyState>(PROTOCOL_SYSTEM);
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use alloy::primitives::U256;
    use num_bigint::BigUint;
    use tycho_client::feed::{synchronizer::ComponentWithState, BlockHeader};
    use tycho_common::{
        dto::{ProtocolComponent, ProtocolStateDelta, ResponseProtocolState},
        models::{token::Token, Chain},
        simulation::protocol_sim::{Balances, BlockContext, ProtocolSim},
        Bytes,
    };

    use super::{
        decoder::{board_key, decode_biconomy_snapshot},
        state::BiconomyState,
    };

    const VENUE: &str = "0x0000009e5f98cdb4dd1e1d2e15b220217c88b3b8";
    const MM: &str = "0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc";
    const PROVIDER: &str = "0xa5d389cb45ac9e4c41329f8edca523e3ffcd4ca1";
    const WETH: &str = "0x4200000000000000000000000000000000000006";
    const USDC: &str = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913";

    fn token(address: &str, symbol: &str, decimals: u32) -> Token {
        Token::new(&Bytes::from(address), symbol, decimals, 0, &[Some(100_000)], Chain::Base, 100)
    }

    /// A provider config attribute: kind byte, signer, vault.
    fn provider_config(kind: u8, signer: &str, vault: Option<&str>) -> Bytes {
        let mut out = vec![kind];
        out.extend(Bytes::from(signer).to_vec());
        out.extend(
            vault
                .map(|v| Bytes::from(v).to_vec())
                .unwrap_or(vec![0u8; 20]),
        );
        Bytes::from(out)
    }

    fn word(value: U256) -> Bytes {
        Bytes::from(value.to_be_bytes::<32>().to_vec())
    }

    /// One maker with a two-level price board WETH -> USDC: 1 WETH at 2000, then 1 more at 1990.
    fn snapshot() -> ComponentWithState {
        let header = U256::from_str_radix(&PROVIDER[2..], 16).unwrap() |
            (U256::from(10_000u64) << 160) |
            (U256::from(2u8) << 200) |
            (U256::from(1u8) << 208);
        let level = |size: u128, price: u128| U256::from(size) | (U256::from(price) << 128);
        let board = |slot: u8| format!("board/{MM}/{WETH}/{USDC}/{slot}");
        let attributes = HashMap::from([
            ("fee_bps".to_owned(), Bytes::from(10u16.to_be_bytes().to_vec())),
            ("maker_count".to_owned(), Bytes::from(vec![1u8])),
            ("maker/0".to_owned(), Bytes::from(MM)),
            (board(0), word(header)),
            (board(2), word(level(1_000_000_000_000_000_000, 2_000_000_000))),
            (board(3), word(level(2_000_000_000_000_000_000, 1_990_000_000))),
            (format!("provider/{PROVIDER}"), provider_config(0, MM, None)),
            (format!("balance/{USDC}/{PROVIDER}"), word(U256::from(5_000_000_000u64))),
        ]);
        ComponentWithState {
            state: ResponseProtocolState {
                component_id: VENUE.to_owned(),
                attributes,
                balances: HashMap::new(),
            }
            .into(),
            component: ProtocolComponent {
                id: VENUE.to_owned(),
                protocol_system: "biconomy".to_owned(),
                tokens: vec![Bytes::from(WETH), Bytes::from(USDC)],
                static_attributes: HashMap::from([("pamm_address".to_owned(), Bytes::from(VENUE))]),
                ..Default::default()
            }
            .into(),
            component_tvl: None,
            entrypoints: Vec::new(),
        }
    }

    fn state() -> BiconomyState {
        let header = BlockHeader { number: 99, timestamp: 1_000, ..Default::default() };
        decode_biconomy_snapshot(&snapshot(), &header).unwrap()
    }

    fn delta(updated: Vec<(String, Bytes)>, deleted: Vec<String>) -> ProtocolStateDelta {
        ProtocolStateDelta {
            component_id: VENUE.to_owned(),
            updated_attributes: updated.into_iter().collect(),
            deleted_attributes: deleted.into_iter().collect(),
        }
    }

    #[test]
    fn quotes_across_levels_net_of_fee() {
        let state = state();
        let weth = token(WETH, "WETH", 18);
        let usdc = token(USDC, "USDC", 6);

        let result = state
            .get_amount_out(BigUint::from(1_500_000_000_000_000_000u64), &weth, &usdc)
            .unwrap();

        // 2000 + 0.5 * 1990 = 2995 USDC gross, minus 10 bps.
        assert_eq!(result.amount, BigUint::from(2_995_000_000u64 - 2_995_000u64));
    }

    #[test]
    fn fill_advances_the_meter_and_spends_inventory() {
        let state = state();
        let weth = token(WETH, "WETH", 18);
        let usdc = token(USDC, "USDC", 6);

        let first = state
            .get_amount_out(BigUint::from(1_000_000_000_000_000_000u64), &weth, &usdc)
            .unwrap();
        let next = first
            .new_state
            .get_amount_out(BigUint::from(1_000_000_000_000_000_000u64), &weth, &usdc)
            .unwrap();

        // The second WETH fills on the 1990 level.
        assert_eq!(next.amount, BigUint::from(1_990_000_000u64 - 1_990_000u64));
        // 5000 USDC of inventory less 2000 and 1990 paid leaves 10 USDC, too little for the
        // next wei of input to be worth anything at 1990.
        let (limit_in, _) = next
            .new_state
            .get_limits(Bytes::from(WETH), Bytes::from(USDC))
            .unwrap();
        assert_eq!(limit_in, BigUint::ZERO);
    }

    #[test]
    fn rejects_sizes_the_venue_cannot_cover() {
        let state = state();
        let weth = token(WETH, "WETH", 18);
        let usdc = token(USDC, "USDC", 6);

        assert!(state
            .get_amount_out(BigUint::from(3_000_000_000_000_000_000u64), &weth, &usdc)
            .is_err());
        assert!(state
            .get_amount_out(BigUint::from(1u8), &usdc, &weth)
            .is_err());
    }

    #[test]
    fn board_expires_with_the_execution_block() {
        let mut state = state();
        let weth = token(WETH, "WETH", 18);
        let usdc = token(USDC, "USDC", 6);

        assert!(state.apply_block(&BlockContext::new(200, 10_001)));
        assert!(state
            .get_amount_out(BigUint::from(1_000u64), &weth, &usdc)
            .is_err());
    }

    #[test]
    fn board_is_dark_when_the_provider_names_another_signer() {
        let mut state = state();
        let weth = token(WETH, "WETH", 18);
        let usdc = token(USDC, "USDC", 6);
        let other = "0x0000000000000000000000000000000000000001";

        state
            .delta_transition(
                delta(
                    vec![(format!("provider/{PROVIDER}"), provider_config(0, other, None))],
                    vec![],
                ),
                &HashMap::new(),
                &Balances::default(),
            )
            .unwrap();

        assert!(state
            .get_amount_out(BigUint::from(1_000u64), &weth, &usdc)
            .is_err());
    }

    #[test]
    fn delta_updates_and_deletes_attributes() {
        let mut state = state();

        state
            .delta_transition(
                delta(
                    vec![(format!("paused/{MM}"), Bytes::from(vec![1u8]))],
                    vec![format!("balance/{USDC}/{PROVIDER}")],
                ),
                &HashMap::new(),
                &Balances::default(),
            )
            .unwrap();

        assert_eq!(state.paused.len(), 1);
        assert_eq!(state.balances.values().copied().max(), Some(U256::ZERO));
    }

    /// The per-block budget across a same-block version bump, replayed through the state the way
    /// the stream delivers it: the words and quotes are `PropAMMExecutor`'s and `PropAMMVenue`'s on
    /// a local deployment (forge 1.5.1, propamm-contracts b97ac8d). Version 1 (1e18 at 2000, 2e18
    /// at 1990, no controls) fills 1.5e18; version 2 lands in the same block, so the default
    /// budget (the version's depth, 2e18) leaves 0.5e18 until the next block.
    #[test]
    fn same_block_version_bump_shares_the_block_budget() {
        const MM: &str = "0x0983366b93822fa5a92eb45463ba30249fa6e11a";
        const PROVIDER: &str = "0xa4ad4f68d0b91cfd19687c881e50f3a00242828c";
        const WETH: &str = "0x5615deb798bb3e4dfa0139dfa1b3d433cc23b72f";
        const USDC: &str = "0x2e234dae75c793f67a35089c9d99245e1c58470b";
        let hex = |value: &str| U256::from_str_radix(value, 16).unwrap();
        let attr = |slot: u8| format!("board/{MM}/{WETH}/{USDC}/{slot}");
        // Thousandths of a token (18 decimals).
        let milli = |units: u128| BigUint::from(units) * BigUint::from(10u8).pow(15);
        let weth = token(WETH, "WETH", 18);
        let usdc = token(USDC, "USDC", 18);
        let address = |value: &str| -> [u8; 20] {
            alloy::hex::decode(value)
                .unwrap()
                .try_into()
                .unwrap()
        };
        let key = board_key(&address(MM), &address(WETH), &address(USDC));
        let words_of = |state: &BiconomyState| state.boards[&key].clone();
        let downcast = |state: &dyn ProtocolSim| {
            state
                .as_any()
                .downcast_ref::<BiconomyState>()
                .unwrap()
                .clone()
        };

        let mut snapshot = snapshot();
        snapshot.state.attributes = HashMap::from([
            ("maker_count".to_owned(), Bytes::from(vec![1u8])),
            ("maker/0".to_owned(), Bytes::from(MM)),
            (
                attr(0),
                word(hex("00000000000102006553f4e8a4ad4f68d0b91cfd19687c881e50f3a00242828c")),
            ),
            (
                attr(1),
                word(hex("00002dc6c000002dc6c000000000000000000000000000000000000000000001")),
            ),
            (
                attr(2),
                word(hex("000000000000006c6b935b8bbd40000000000000000000000de0b6b3a7640000")),
            ),
            (
                attr(3),
                word(hex("000000000000006be0cc38873358000000000000000000001bc16d674ec80000")),
            ),
            (format!("provider/{PROVIDER}"), provider_config(0, MM, None)),
            (format!("balance/{USDC}/{PROVIDER}"), word(U256::from(10u8).pow(U256::from(24u8)))),
        ]);
        // Executes in block 3,000,001.
        let header =
            BlockHeader { number: 3_000_000, timestamp: 1_699_999_998, ..Default::default() };
        let state = decode_biconomy_snapshot(&snapshot, &header).unwrap();

        let first = state
            .get_amount_out(milli(1_500), &weth, &usdc)
            .unwrap();
        assert_eq!(first.amount, milli(2_995_000));
        // The fill writes both meters exactly as the executor stored them.
        let mut state = downcast(first.new_state.as_ref());
        let words = words_of(&state);
        assert_eq!(
            words[&1],
            hex("00002dc6c000002dc6c0000000000000000014d1120d7b160000000000000001")
        );
        assert_eq!(
            words[&23],
            hex("0000000000000000000000000000000000000014d1120d7b16000000002dc6c1")
        );

        // Version 2 (1e18 at 2001, 2e18 at 1991) is committed in the same block.
        state
            .delta_transition(
                delta(
                    vec![
                        (
                            attr(1),
                            word(hex(
                                "00002dc6c100002dc6c100000000000000000000000000000000000000000002",
                            )),
                        ),
                        (
                            attr(2),
                            word(hex(
                                "000000000000006c7974123f64a4000000000000000000000de0b6b3a7640000",
                            )),
                        ),
                        (
                            attr(3),
                            word(hex(
                                "000000000000006beeacef3adabc000000000000000000001bc16d674ec80000",
                            )),
                        ),
                    ],
                    vec![],
                ),
                &HashMap::new(),
                &Balances::default(),
            )
            .unwrap();

        assert_eq!(
            state
                .get_limits(Bytes::from(WETH), Bytes::from(USDC))
                .unwrap(),
            (milli(500), milli(1_000_500))
        );
        assert!(state
            .get_amount_out(milli(500) + 1u8, &weth, &usdc)
            .is_err());
        let second = state
            .get_amount_out(milli(500), &weth, &usdc)
            .unwrap();
        assert_eq!(second.amount, milli(1_000_500));
        let mut state = downcast(second.new_state.as_ref());
        let words = words_of(&state);
        assert_eq!(
            words[&1],
            hex("00002dc6c100002dc6c1000000000000000006f05b59d3b20000000000000002")
        );
        assert_eq!(
            words[&23],
            hex("000000000000000000000000000000000000001bc16d674ec8000000002dc6c1")
        );
        assert!(state
            .get_amount_out(BigUint::from(1u8), &weth, &usdc)
            .is_err());

        // The next block re-opens the version's depth from its meter.
        assert!(state.apply_block(&BlockContext::new(3_000_002, 1_700_000_000)));
        assert_eq!(
            state
                .get_amount_out(milli(1_500), &weth, &usdc)
                .unwrap()
                .amount,
            milli(2_991_500)
        );
    }

    /// Rebuilds the state from the executor's and venue's raw storage on Base Sepolia, the words
    /// the substreams package indexes, and checks every maker's board against
    /// `PropAMMExecutor.board()`, and every size against `PropAMMVenue.quote` and `quoteFillable`
    /// (and the limits against `quoteFillable` of everything) at the same block. Needs
    /// `BASE_SEPOLIA_RPC_URL` (an archive node) and the deployment in `deployment` below.
    #[cfg(feature = "network_tests")]
    mod network {
        use std::{collections::BTreeMap, env, str::FromStr};

        use alloy::{
            eips::BlockId,
            primitives::{keccak256, Address, U256},
            providers::{Provider, ProviderBuilder},
            rpc::types::TransactionRequest,
            sol,
            sol_types::SolCall,
        };
        use futures::future::join_all;

        use super::*;
        use crate::evm::protocol::{
            biconomy::{
                decoder::{apply_attributes, board_key, hex_address, AttributeChange},
                math::{after_protocol_fee, merge, BOARD_WORDS},
                state::BlockEnvState,
            },
            u256_num::{biguint_to_u256, u256_to_biguint},
        };

        /// The deployment the test reads, the one place to edit for a new generation.
        mod deployment {
            pub const UNSET: &str = "0x0000000000000000000000000000000000000000";
            /// `PropAMMVenue` on Base Sepolia (84532), release candidate v1.0.0-rc.1 (efcb89b).
            pub const VENUE: &str = "0x000000d88b78dC90957BaAF474E771f739926994";
            /// `PropAMMExecutor` on Base Sepolia (84532), release candidate v1.0.0-rc.1.
            pub const EXECUTOR: &str = "0x000000B46d7076bc859572154786e2858E1f9A41";
            /// Blocks to compare at, across the release candidate's end-to-end run (200 ms
            /// blocks): the price board's commit (48836773), the offset board's commit before
            /// its anchor (48836779) and with it (48836784), a venue swap's block and the next
            /// (48836804, 48836805), a hosted fill (48836819), the controls commit inside its
            /// one-block premium (48836824) and after (48836825), a venue swap under the
            /// controls' block cap and the next block (48836841, 48836842), then widening ages
            /// of up to ~1,700 blocks.
            pub const BLOCKS: &[u64] = &[
                48_836_773, 48_836_779, 48_836_784, 48_836_790, 48_836_804, 48_836_805, 48_836_819,
                48_836_824, 48_836_825, 48_836_841, 48_836_842, 48_836_900, 48_837_000, 48_837_500,
                48_838_000, 48_838_500,
            ];
            /// Tokens of the venue's pairs: address, symbol, decimals and the `balanceOf` and
            /// `allowance` mapping slots. Base Sepolia MockWETH and MockUSDC are OpenZeppelin
            /// ERC20s (slots 0 and 1), both with 18 decimals.
            pub const TOKENS: &[(&str, &str, u32, u8, u8)] = &[
                ("0x8b414ad7005eefd315af2a16538885eae229bab7", "WETH", 18, 0, 1),
                ("0xabbdbbbd6d56593a9c5656c06cb30d61e4a544df", "USDC", 18, 0, 1),
            ];
            /// Sizes to quote, by index into `TOKENS`: tokenIn, tokenOut, amounts in.
            pub const QUOTES: &[(usize, usize, &[u128])] = &[
                (
                    0,
                    1,
                    &[
                        1,
                        1_000_000,
                        1_000_000_000_000,
                        1_000_000_000_000_000,
                        100_000_000_000_000_000,
                        500_000_000_000_000_000,
                        600_000_000_000_000_001,
                        1_000_000_000_000_000_000,
                        2_000_000_000_000_000_000,
                        3_000_000_000_000_000_000,
                        6_000_000_000_000_000_000,
                    ],
                ),
                (1, 0, &[1, 1_000_000_000_000_000_000, 2_000_000_000_000_000_000_000]),
            ];
        }

        /// `makerList` sits at venue slot 2 since the venue gained `selfReporting` at slot 1.
        const VENUE_MAKERS_SLOT: u8 = 2;

        sol! {
            function quote(address tokenIn, address tokenOut, uint256 amountIn) external view returns (uint256);
            function quoteFillable(address tokenIn, address tokenOut, uint256 amountIn) external view returns (uint256 fillableAmountIn, uint256 amountOut);
            function board(address mm, address tokenIn, address tokenOut) external view returns (uint256[] memory sizes, uint256[] memory prices, uint256 filled, uint256 remaining, uint256 expiresAt, uint256 nonce, uint256 anchorNonce, uint8 mode);
            function available(address token) external view returns (uint256);
            function signer() external view returns (address);
            function vault() external view returns (address);
        }

        fn slot(key: Address, base: U256) -> U256 {
            let mut buf = [0u8; 64];
            buf[12..32].copy_from_slice(key.as_slice());
            buf[32..].copy_from_slice(&base.to_be_bytes::<32>());
            U256::from_be_bytes(keccak256(buf).0)
        }

        async fn call<P: Provider>(
            provider: &P,
            to: Address,
            data: Vec<u8>,
            block: u64,
        ) -> Option<Vec<u8>> {
            let tx = TransactionRequest::default()
                .to(to)
                .input(data.into());
            provider
                .call(tx)
                .block(BlockId::number(block))
                .await
                .ok()
                .map(|out| out.to_vec())
        }

        async fn storage<P: Provider>(
            provider: &P,
            contract: Address,
            slot: U256,
            block: u64,
        ) -> U256 {
            provider
                .get_storage_at(contract, slot)
                .block_id(BlockId::number(block))
                .await
                .unwrap()
        }

        fn tokens() -> Vec<(Token, u8, u8)> {
            deployment::TOKENS
                .iter()
                .map(|(address, symbol, decimals, balance_slot, allowance_slot)| {
                    (token(address, symbol, *decimals), *balance_slot, *allowance_slot)
                })
                .collect()
        }

        fn address_of(token: &Token) -> Address {
            Address::from_slice(token.address.as_ref())
        }

        /// `PropAMMVenue.quoteFillable` replayed on the state: the covered part of `amount_in`
        /// and its output net of the fee, re-planned at the covered amount when short.
        fn quote_fillable(
            state: &BiconomyState,
            token_in: &Token,
            token_out: &Token,
            amount_in: U256,
        ) -> (U256, U256) {
            if amount_in.is_zero() {
                return (U256::ZERO, U256::ZERO);
            }
            let (_, views) = state
                .board_views(&address_of(token_in).0 .0, &address_of(token_out).0 .0)
                .unwrap();
            let mut plan = merge(&views, amount_in);
            if plan.covered < amount_in {
                let capacity = plan.covered;
                if capacity.is_zero() {
                    return (U256::ZERO, U256::ZERO);
                }
                plan = merge(&views, capacity);
                if plan.covered != capacity {
                    return (U256::ZERO, U256::ZERO);
                }
            }
            if plan.out.is_zero() {
                return (U256::ZERO, U256::ZERO);
            }
            (plan.covered, after_protocol_fee(plan.out, state.fee_bps))
        }

        /// One board's 24 words; only the header is read for a board never committed.
        async fn board_words<P: Provider>(
            provider: &P,
            executor: Address,
            base: U256,
            block: u64,
        ) -> BTreeMap<u8, U256> {
            let mut words = BTreeMap::new();
            let header = storage(provider, executor, base, block).await;
            if header.is_zero() {
                return words;
            }
            words.insert(0, header);
            let rest = join_all(
                (1..BOARD_WORDS).map(|i| storage(provider, executor, base + U256::from(i), block)),
            )
            .await;
            for (i, value) in (1..BOARD_WORDS).zip(rest) {
                if !value.is_zero() {
                    words.insert(i, value);
                }
            }
            words
        }

        /// The state the substreams package indexes, rebuilt from storage at `block`: venue
        /// slots, board and anchor words, and token balances and allowances behind each
        /// provider. Provider configs (kind, signer, vault) are what the package takes as params.
        async fn state_at<P: Provider>(
            provider: &P,
            block: u64,
            tokens: &[(Token, u8, u8)],
        ) -> (BiconomyState, Vec<Address>) {
            let venue = Address::from_str(deployment::VENUE).unwrap();
            let executor = Address::from_str(deployment::EXECUTOR).unwrap();
            let header = provider
                .get_block_by_number(block.into())
                .await
                .unwrap()
                .unwrap()
                .header;

            let mut state = BiconomyState {
                block: BlockEnvState { number: block, timestamp: header.timestamp },
                ..Default::default()
            };
            let makers_slot = U256::from(VENUE_MAKERS_SLOT);
            let count = storage(provider, venue, makers_slot, block).await;
            let mut attributes: Vec<(String, Bytes)> = vec![
                ("fee_bps".to_owned(), word(storage(provider, venue, U256::ZERO, block).await)),
                ("maker_count".to_owned(), word(count)),
            ];
            let first = U256::from_be_bytes(keccak256(makers_slot.to_be_bytes::<32>()).0);
            let entries = join_all(
                (0..count.to::<u64>())
                    .map(|i| storage(provider, venue, first + U256::from(i), block)),
            )
            .await;
            let mut makers = Vec::new();
            for (i, value) in entries.into_iter().enumerate() {
                makers.push(Address::from_slice(&value.to_be_bytes::<32>()[12..]));
                attributes.push((format!("maker/{i}"), word(value)));
            }

            let addresses: Vec<Address> = tokens
                .iter()
                .map(|(token, _, _)| address_of(token))
                .collect();
            for mm in &makers {
                if !storage(provider, executor, slot(*mm, U256::from(2u8)), block)
                    .await
                    .is_zero()
                {
                    state.paused.insert(mm.0 .0);
                }
                for (i, a) in addresses.iter().enumerate() {
                    for b in &addresses[i + 1..] {
                        let (t0, t1) = if a < b { (*a, *b) } else { (*b, *a) };
                        let anchor = storage(
                            provider,
                            executor,
                            slot(t1, slot(t0, slot(*mm, U256::from(1u8)))),
                            block,
                        )
                        .await;
                        state
                            .anchors
                            .insert(board_key(&mm.0 .0, &t0.0 .0, &t1.0 .0), anchor);
                        for (tin, tout) in [(*a, *b), (*b, *a)] {
                            let base = slot(tout, slot(tin, slot(*mm, U256::ZERO)));
                            let words = board_words(provider, executor, base, block).await;
                            let board_provider = Address::from_slice(
                                &words
                                    .get(&0)
                                    .copied()
                                    .unwrap_or_default()
                                    .to_be_bytes::<32>()[12..],
                            );
                            state
                                .boards
                                .insert(board_key(&mm.0 .0, &tin.0 .0, &tout.0 .0), words);
                            if board_provider.is_zero() {
                                continue;
                            }
                            let key = format!("provider/{}", hex_address(&board_provider.0 .0));
                            if attributes
                                .iter()
                                .any(|(name, _)| *name == key)
                            {
                                continue;
                            }
                            let signer =
                                call(provider, board_provider, signerCall {}.abi_encode(), block)
                                    .await
                                    .and_then(|out| signerCall::abi_decode_returns(&out).ok())
                                    .unwrap_or_default();
                            let vault =
                                call(provider, board_provider, vaultCall {}.abi_encode(), block)
                                    .await
                                    .and_then(|out| vaultCall::abi_decode_returns(&out).ok())
                                    .filter(|vault| !vault.is_zero());
                            let available = call(
                                provider,
                                board_provider,
                                availableCall { token: tout }.abi_encode(),
                                block,
                            )
                            .await
                            .and_then(|out| availableCall::abi_decode_returns(&out).ok())
                            .unwrap_or(U256::MAX);
                            let kind: u8 = match vault {
                                Some(_) => 1,
                                None if available >= U256::from(u128::MAX) => 2,
                                None => 0,
                            };
                            let mut config = vec![kind];
                            config.extend(signer.0 .0);
                            config.extend(vault.unwrap_or_default().0 .0);
                            attributes.push((key, Bytes::from(config)));
                            for (token, balance_slot, allowance_slot) in tokens {
                                let token = address_of(token);
                                let holder = vault.unwrap_or(board_provider);
                                if kind != 2 {
                                    let value = storage(
                                        provider,
                                        token,
                                        slot(holder, U256::from(*balance_slot)),
                                        block,
                                    )
                                    .await;
                                    attributes.push((
                                        format!(
                                            "balance/{}/{}",
                                            hex_address(&token.0 .0),
                                            hex_address(&holder.0 .0)
                                        ),
                                        word(value),
                                    ));
                                }
                                if let Some(vault) = vault {
                                    let value = storage(
                                        provider,
                                        token,
                                        slot(
                                            board_provider,
                                            slot(vault, U256::from(*allowance_slot)),
                                        ),
                                        block,
                                    )
                                    .await;
                                    attributes.push((
                                        format!(
                                            "allowance/{}/{}/{}",
                                            hex_address(&token.0 .0),
                                            hex_address(&vault.0 .0),
                                            hex_address(&board_provider.0 .0)
                                        ),
                                        word(value),
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            apply_attributes(
                &mut state,
                attributes
                    .iter()
                    .map(|(name, value)| AttributeChange::Set(name, value)),
            )
            .unwrap();
            (state, makers)
        }

        #[tokio::test]
        // `BLOCKS` is a hand-edited constant, empty until a generation is deployed.
        #[allow(clippy::const_is_empty)]
        async fn matches_executor_boards_and_venue_quotes_on_base_sepolia() {
            assert!(
                deployment::VENUE != deployment::UNSET &&
                    deployment::EXECUTOR != deployment::UNSET &&
                    !deployment::BLOCKS.is_empty(),
                "fill in the Base Sepolia deployment in `network::deployment`"
            );
            let url = env::var("BASE_SEPOLIA_RPC_URL").expect("BASE_SEPOLIA_RPC_URL");
            let provider = ProviderBuilder::new().connect_http(url.parse().unwrap());
            let provider = &provider;
            let venue = Address::from_str(deployment::VENUE).unwrap();
            let executor = Address::from_str(deployment::EXECUTOR).unwrap();
            let tokens = tokens();

            let (mut boards, mut live_boards) = (0, 0);
            let (mut quotes, mut live_quotes, mut fillables, mut limits) = (0, 0, 0, 0);
            for &block in deployment::BLOCKS {
                let (state, makers) = state_at(provider, block, &tokens).await;
                for &(i, o, sizes) in deployment::QUOTES {
                    let (tin, tout) = (&tokens[i].0, &tokens[o].0);
                    let route = format!("block {block} {} -> {}", tin.symbol, tout.symbol);

                    // Every maker's board as the venue reads it.
                    let (_, views) = state
                        .board_views(&address_of(tin).0 .0, &address_of(tout).0 .0)
                        .unwrap();
                    let chain_boards = join_all(makers.iter().map(|mm| {
                        let data = boardCall {
                            mm: *mm,
                            tokenIn: address_of(tin),
                            tokenOut: address_of(tout),
                        }
                        .abi_encode();
                        call(provider, executor, data, block)
                    }))
                    .await;
                    for ((mm, view), chain) in makers
                        .iter()
                        .zip(&views)
                        .zip(chain_boards)
                    {
                        let chain = boardCall::abi_decode_returns(&chain.unwrap()).unwrap();
                        let context = format!("{route} board of {mm}");
                        let level_sizes: Vec<U256> = view
                            .levels
                            .iter()
                            .map(|level| level.size)
                            .collect();
                        let level_prices: Vec<U256> = view
                            .levels
                            .iter()
                            .map(|level| level.price)
                            .collect();
                        assert_eq!(level_sizes, chain.sizes, "{context} sizes");
                        assert_eq!(level_prices, chain.prices, "{context} prices");
                        assert_eq!(view.filled, chain.filled, "{context} filled");
                        assert_eq!(view.remaining, chain.remaining, "{context} remaining");
                        boards += 1;
                        live_boards += usize::from(!chain.remaining.is_zero());
                    }

                    // `quote` and `quoteFillable` for every size.
                    let mut amounts: Vec<U256> = sizes
                        .iter()
                        .map(|size| U256::from(*size))
                        .collect();
                    amounts.push(U256::MAX);
                    let chain_quotes = join_all(amounts.iter().map(|amount| {
                        let data = quoteCall {
                            tokenIn: address_of(tin),
                            tokenOut: address_of(tout),
                            amountIn: *amount,
                        }
                        .abi_encode();
                        call(provider, venue, data, block)
                    }))
                    .await;
                    let chain_fillables = join_all(amounts.iter().map(|amount| {
                        let data = quoteFillableCall {
                            tokenIn: address_of(tin),
                            tokenOut: address_of(tout),
                            amountIn: *amount,
                        }
                        .abi_encode();
                        call(provider, venue, data, block)
                    }))
                    .await;
                    for ((amount, chain_quote), chain_fillable) in amounts
                        .iter()
                        .zip(chain_quotes)
                        .zip(chain_fillables)
                    {
                        let chain_fillable =
                            quoteFillableCall::abi_decode_returns(&chain_fillable.unwrap())
                                .unwrap();
                        let chain_fillable =
                            (chain_fillable.fillableAmountIn, chain_fillable.amountOut);
                        if *amount == U256::MAX {
                            // Tycho's limits are the venue's whole fillable size.
                            let (limit_in, limit_out) = state
                                .get_limits(tin.address.clone(), tout.address.clone())
                                .unwrap();
                            assert_eq!(
                                (biguint_to_u256(&limit_in), biguint_to_u256(&limit_out)),
                                chain_fillable,
                                "{route} limits"
                            );
                            limits += 1;
                            continue;
                        }
                        let chain_quote =
                            chain_quote.map(|out| quoteCall::abi_decode_returns(&out).unwrap());
                        let sim_quote = state
                            .get_amount_out(u256_to_biguint(*amount), tin, tout)
                            .ok()
                            .map(|result| biguint_to_u256(&result.amount));
                        assert_eq!(sim_quote, chain_quote, "{route} quote of {amount}");
                        assert_eq!(
                            quote_fillable(&state, tin, tout, *amount),
                            chain_fillable,
                            "{route} quoteFillable of {amount}"
                        );
                        quotes += 1;
                        fillables += 1;
                        live_quotes += usize::from(chain_quote.is_some());
                    }
                }
            }
            println!(
                "{} blocks: {boards} boards ({live_boards} live), {quotes} quotes \
                 ({live_quotes} live), {fillables} quoteFillable, {limits} limits, all equal",
                deployment::BLOCKS.len()
            );
            assert!(live_boards > 0 && live_quotes > 0, "nothing was live at the pinned blocks");
        }
    }
}
