use frame_support::dispatch::DispatchResult;
use frame_support::weights::Weight;
use sp_std::vec::Vec;
use xcm::{latest::prelude::*, DoubleEncoded};

/// Abstraction over cross-chain token transfers.
pub trait XcmTransfer<AccountId, Balance, CurrencyId> {
	/// Transfer native currencies.
	fn transfer(
		who: AccountId,
		currency_id: CurrencyId,
		amount: Balance,
		dest: MultiLocation,
		dest_weight: Weight,
		maybe_call: Option<Vec<u8>>,
		//maybe_transact_call: Option<DoubleEncoded<Call>>,
		maybe_transact_fee: Balance,
	) -> DispatchResult;

	/// Transfer `MultiAsset`
	fn transfer_multi_asset(
		who: AccountId,
		asset: MultiAsset,
		dest: MultiLocation,
		dest_weight: Weight,
	) -> DispatchResult;
}
