# base-biconomy

Indexes the Biconomy PropAMM venue on Base as one component. The venue is a push-payment
`IPropAMM` contract that serves several pairs from one address and fills against the boards
makers keep in the PropAMM executor.

| | Base |
|---|---|
| Venue (`PropAMMVenue`) | `0x0000009E5F98cDB4dd1E1d2E15B220217c88B3b8` |
| Executor (`PropAMMExecutor`) | `0x0000006f9bBA066e4efda2e0CAbE4FF8cf9030aC` |
| Tokens | WETH `0x4200000000000000000000000000000000000006`, USDC `0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913`, cbBTC `0xcbB7C0000aB88B473b1f5aFd9ef808440eed33Bf` |
| Executor deployed | block 52547671 |
| Venue deployed | block 52547680 |

## Component

- id: the venue address
- tokens: every token the venue lists in `getPairs()`
- static attributes: `pamm_address` (the venue) and `executor`
- protocol system `biconomy`, type `biconomy_venue`
- balances: per token, the inventory the configured providers can pay out (see below). The venue
  itself holds nothing between swaps.

## State

The `biconomy` state in `tycho-simulation` decodes these attributes and prices a swap exactly
as `PropAMMVenue.quote` does at the execution block. The package makes no RPC calls: every
value comes from storage changes or from the package params, so it runs on Flashblocks.

| attribute | source |
|---|---|
| `board/{mm}/{tokenIn}/{tokenOut}/{slot}` | executor storage word `slot` (0 to 23) of `boards[mm][tokenIn][tokenOut]` |
| `anchor/{mm}/{token0}/{token1}` | executor storage word of `anchors[mm][token0][token1]` |
| `paused/{mm}` | executor storage word of `makerPaused[mm]` |
| `fee_bps` | venue storage slot 0 (`feeBps` is its low 16 bits) |
| `maker_count`, `maker/{i}` | venue `makerList` length (slot 1) and elements |
| `provider/{provider}` | provider config from params: kind and vault |
| `signer/{provider}` | the provider's `signer` storage word (bootstrap value from params) |
| `approved_executor/{provider}` | the provider's `approvedExecutor` storage word (bootstrap value: `executor`) |
| `router_count/{vault}`, `router/{vault}` | the vault's router array length and first element, which `router()` returns |
| `balance/{token}/{holder}` | the token's `balanceOf` storage word for a provider or vault |
| `allowance/{token}/{owner}/{spender}` | the token's `allowance` storage word, vault to provider |

Board, anchor and pause words come from the transaction's storage changes. Every write to them
comes with an executor event naming the maker and pair (`LadderCommitted`, `OffsetsCommitted`,
`AnchorCommitted`, `ControlsCommitted`, `MMFillExecuted`, `MakerPaused`), which is how a
storage slot is matched to its board.

Commits on the executor are permissionless, so the package keeps an event only when its maker is
in the venue's `makerList` at that transaction and both tokens are in the `tokens` param. The
venue lists a maker before the maker commits, so nothing is lost in the normal flow. A maker that
commits while unlisted and is listed later has those boards missing until its next commit.

`map_state_writes` collects the venue, provider, vault and token writes and `store_state` keeps
their latest values, which `map_protocol_changes` reads to check the maker list and to compute
balances at each transaction.

## Provider inventory, signer and executor

The executor caps a board's depth at its provider's `available(tokenOut)` and keeps a board dark
unless the provider's `signer()` is the maker. A provider that approves another executor still
shows a quote on the venue, but the fill reverts. The simulation keeps a board dark unless the
provider names the maker as signer and approves `executor`, and computes inventory from the
provider config in the params and the tracked storage:

| kind | `available(token)` |
|---|---|
| `balance` | the provider's own token balance |
| `vault` | the smaller of the vault's balance and its allowance to the provider, and zero unless the vault's `router()` is the provider |
| `unbounded` | no cap |

The component balance of each token is the sum of `available(token)` over the configured
providers (unbounded providers add nothing), emitted whenever a tracked balance, allowance or
router changes.

Providers whose owner can change the signer or executor are listed in `provider_slots` with the
two storage slots, so the package follows those writes. Providers without an entry are taken as
immutable: the signer in `providers` and `executor` hold for good. A vault in `vault_routers`
keeps `router()` as the first element of a dynamic array at the given slot (slot 2 for the
Tempest vault), and the package follows that array. Both providers configured today keep the
signer in slot 0 and `approvedExecutor` in slot 1.

Token balance and allowance slots are given per token in the `tokens` param (WETH 3 and 4, USDC
and cbBTC 9 and 10 on Base; USDC keeps a blacklist flag in the top bit of the balance word,
which the simulation masks). Balances and allowances that predate `bootstrap_block` go in the
`balances` and `allowances` params; none do for the providers configured today.

A provider that is not in the params has no inventory, so its boards stay dark: the package can
miss a quote but never overstates depth.

## Adding a provider or a token

Provider configs, bootstrap values and the component's tokens are emitted once, in the creation
transaction. A running extractor does not pick up a params change, so adding a provider, a vault
or a token means:

1. Add it to the params (`providers`, and `provider_slots` or `vault_routers` where they apply;
   `tokens` for a new token).
2. Add `balances` and `allowances` bootstrap values for any provider or vault that held funds at
   `bootstrap_block`. A provider deployed after it needs none: its storage writes are indexed.
3. Resync the package from block 52547671.

Biconomy deploys a new provider when a maker rotates its signer or moves to a new executor
generation, so each of these is a resync.

## Update timing

Updates arrive per flashblock on Tycho's Base stream. Prices also move with time inside a board
(anchor drift, quote-age widening, premium window, expiry), which the simulation applies from
the execution block's number and timestamp without a state update.
