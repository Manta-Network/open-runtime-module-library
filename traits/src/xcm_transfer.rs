use frame_support::dispatch::DispatchResult;
use xcm::latest::prelude::*;

/// Abstraction over cross-chain token transfers.
pub trait XcmTransfer<AccountId, Balance, CurrencyId> {
	/// Transfer native currencies.
	fn transfer(
		who: AccountId,
		currency_id: CurrencyId,
		amount: Balance,
		dest: MultiLocation,
		dest_weight_limit: WeightLimit,
	) -> DispatchResult;

	/// Transfer `MultiAsset`
	fn transfer_multi_asset(
		who: AccountId,
		asset: MultiAsset,
		dest: MultiLocation,
		dest_weight_limit: WeightLimit,
	) -> DispatchResult;

	/// Transfer `MultiAssetWithFee`
	fn transfer_multiasset_with_fee(
		who: AccountId,
		asset: MultiAsset,
		fee: MultiAsset,
		dest: MultiLocation,
		dest_weight_limit: WeightLimit,
	) -> DispatchResult;
}

pub trait NativeBarrier<AccountId, Balance> {
	fn update_xcm_native_transfers(account_id: &AccountId, amount: Balance);
	fn ensure_xcm_transfer_limit_not_exceeded(account_id: &AccountId, amount: Balance) -> DispatchResult;
}

pub trait NativeChecker<CurrencyId> {
	fn is_native(currency_id: CurrencyId) -> bool;
}
