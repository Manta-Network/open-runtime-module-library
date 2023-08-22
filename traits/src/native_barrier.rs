use frame_support::dispatch::DispatchResult;
pub trait NativeBarrier<AccountId, Balance> {
	fn update_native_barrier(account_id: &AccountId, amount: Balance);
	fn ensure_limit_not_exceeded(account_id: &AccountId, amount: Balance) -> DispatchResult;
}
pub trait NativeChecker<CurrencyId> {
	fn is_native(currency_id: &CurrencyId) -> bool;
}
