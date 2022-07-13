//! # Xtokens Module
//!
//! ## Overview
//!
//! The xtokens module provides cross-chain token transfer functionality, by
//! cross-consensus messages(XCM).
//!
//! The xtokens module provides functions for
//! - Token transfer from parachains to relay chain.
//! - Token transfer between parachains, including relay chain tokens like DOT,
//!   KSM, and parachain tokens like ACA, aUSD.
//!
//! ## Interface
//!
//! ### Dispatchable functions
//!
//! - `transfer`: Transfer local assets with given `CurrencyId` and `Amount`.
//! - `transfer_multiasset`: Transfer `MultiAsset` assets.

#![cfg_attr(not(feature = "std"), no_std)]
#![allow(clippy::from_over_into)]
#![allow(clippy::unused_unit)]
#![allow(clippy::large_enum_variant)]

use frame_support::{
	log,
	pallet_prelude::*,
	require_transactional,
	traits::{Contains, Get},
	transactional, Parameter,
};
use frame_system::{ensure_signed, pallet_prelude::*};
use sp_runtime::{
	traits::{AtLeast32BitUnsigned, Convert, MaybeSerializeDeserialize, Member, Zero},
	DispatchError,
};
use sp_std::{prelude::*, result::Result};

use xcm::prelude::*;
use xcm_executor::traits::{InvertLocation, WeightBounds};

pub use module::*;
use orml_traits::{
	location::{Parse, Reserve},
	GetByKey, XcmTransfer,
};

mod mock;
mod tests;

enum TransferKind {
	/// Transfer self reserve asset.
	SelfReserveAsset,
	/// To reserve location.
	ToReserve,
	/// To non-reserve location.
	ToNonReserve,
}
use TransferKind::*;

#[frame_support::pallet]
pub mod module {
	use super::*;

	#[pallet::config]
	pub trait Config: frame_system::Config {
		type Event: From<Event<Self>> + IsType<<Self as frame_system::Config>::Event>;

		/// The balance type.
		type Balance: Parameter
			+ Member
			+ AtLeast32BitUnsigned
			+ Default
			+ Copy
			+ MaybeSerializeDeserialize
			+ Into<u128>;

		/// Currency Id.
		type CurrencyId: Parameter + Member + Clone;

		/// Convert `T::CurrencyId` to `MultiLocation`.
		type CurrencyIdConvert: Convert<Self::CurrencyId, Option<MultiLocation>>;

		/// Convert `T::AccountId` to `MultiLocation`.
		type AccountIdToMultiLocation: Convert<Self::AccountId, MultiLocation>;

		/// Self chain location.
		#[pallet::constant]
		type SelfLocation: Get<MultiLocation>;

		/// Minimum xcm execution fee paid on destination chain.
		type MinXcmFee: GetByKey<MultiLocation, Option<u128>>;

		/// XCM executor.
		type XcmExecutor: ExecuteXcm<Self::Call>;

		/// MultiLocation filter
		type MultiLocationsFilter: Contains<MultiLocation>;

		/// Means of measuring the weight consumed by an XCM message locally.
		type Weigher: WeightBounds<Self::Call>;

		/// Base XCM weight.
		///
		/// The actually weight for an XCM message is `T::BaseXcmWeight +
		/// T::Weigher::weight(&msg)`.
		#[pallet::constant]
		type BaseXcmWeight: Get<Weight>;

		/// Means of inverting a location.
		type LocationInverter: InvertLocation;

		/// The maximum number of distinct assets allowed to be transferred in a
		/// single helper extrinsic.
		type MaxAssetsForTransfer: Get<usize>;

		/// The way to retreave the reserve of a MultiAsset. This can be
		/// configured to accept absolute or relative paths for self tokens
		type ReserveProvider: Reserve;

		type XcmSender: SendXcm;

		#[pallet::constant]
		type MaxTransactSize: Get<u32>;
	}

	#[pallet::event]
	#[pallet::generate_deposit(fn deposit_event)]
	pub enum Event<T: Config> {
		/// Transferred `MultiAsset` with fee.
		TransferredMultiAssets {
			sender: T::AccountId,
			assets: MultiAssets,
			fee: MultiAsset,
			dest: MultiLocation,
		},
	}

	#[pallet::error]
	pub enum Error<T> {
		/// Asset has no reserve location.
		AssetHasNoReserve,
		/// Not cross-chain transfer.
		NotCrossChainTransfer,
		/// Invalid transfer destination.
		InvalidDest,
		/// Currency is not cross-chain transferable.
		NotCrossChainTransferableCurrency,
		/// The message's weight could not be determined.
		UnweighableMessage,
		// TODO: expand into XcmExecutionFailed(XcmError) after https://github.com/paritytech/substrate/pull/10242 done
		/// XCM execution failed.
		XcmExecutionFailed,
		/// Could not re-anchor the assets to declare the fees for the
		/// destination chain.
		CannotReanchor,
		/// Could not get ancestry of asset reserve location.
		InvalidAncestry,
		/// The MultiAsset is invalid.
		InvalidAsset,
		/// The destination `MultiLocation` provided cannot be inverted.
		DestinationNotInvertible,
		/// The version of the `Versioned` value used is not able to be
		/// interpreted.
		BadVersion,
		/// We tried sending distinct asset and fee but they have different
		/// reserve chains.
		DistinctReserveForAssetAndFee,
		/// The fee is zero.
		ZeroFee,
		/// The transfering asset amount is zero.
		ZeroAmount,
		/// The number of assets to be sent is over the maximum.
		TooManyAssetsBeingSent,
		/// The specified index does not exist in a MultiAssets struct.
		AssetIndexNonExistent,
		/// Fee is not enough.
		FeeNotEnough,
		/// Not supported MultiLocation
		NotSupportedMultiLocation,
		/// MinXcmFee not registered for certain reserve location
		MinXcmFeeNotDefined,
		SendFailure,
		TransactTooLarge,
	}

	#[pallet::hooks]
	impl<T: Config> Hooks<T::BlockNumber> for Pallet<T> {}

	#[pallet::pallet]
	pub struct Pallet<T>(_);

	#[pallet::call]
	impl<T: Config> Pallet<T> {
		/// Transfer native currencies.
		///
		/// `dest_weight` is the weight for XCM execution on the dest chain, and
		/// it would be charged from the transferred assets. If set below
		/// requirements, the execution may fail and assets wouldn't be
		/// received.
		///
		/// It's a no-op if any error on local XCM execution or message sending.
		/// Note sending assets out per se doesn't guarantee they would be
		/// received. Receiving depends on if the XCM message could be delivered
		/// by the network, and if the receiving chain would handle
		/// messages correctly.
		#[pallet::weight(Pallet::<T>::weight_of_transfer(currency_id.clone(), *amount, dest))]
		#[transactional]
		pub fn transfer(
			origin: OriginFor<T>,
			currency_id: T::CurrencyId,
			amount: T::Balance,
			dest: Box<VersionedMultiLocation>,
			dest_weight: Weight,
		) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let dest: MultiLocation = (*dest).try_into().map_err(|()| Error::<T>::BadVersion)?;
			Self::do_transfer(who, currency_id, amount, dest, dest_weight)
		}

		/// Transfer `MultiAsset`.
		///
		/// `dest_weight` is the weight for XCM execution on the dest chain, and
		/// it would be charged from the transferred assets. If set below
		/// requirements, the execution may fail and assets wouldn't be
		/// received.
		///
		/// It's a no-op if any error on local XCM execution or message sending.
		/// Note sending assets out per se doesn't guarantee they would be
		/// received. Receiving depends on if the XCM message could be delivered
		/// by the network, and if the receiving chain would handle
		/// messages correctly.
		#[pallet::weight(Pallet::<T>::weight_of_transfer_multiasset(asset, dest))]
		#[transactional]
		pub fn transfer_multiasset(
			origin: OriginFor<T>,
			asset: Box<VersionedMultiAsset>,
			dest: Box<VersionedMultiLocation>,
			dest_weight: Weight,
		) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let asset: MultiAsset = (*asset).try_into().map_err(|()| Error::<T>::BadVersion)?;
			let dest: MultiLocation = (*dest).try_into().map_err(|()| Error::<T>::BadVersion)?;
			Self::do_transfer_multiasset(who, asset, dest, dest_weight)
		}

		/// Transfer native currencies specifying the fee and amount as
		/// separate.
		///
		/// `dest_weight` is the weight for XCM execution on the dest chain, and
		/// it would be charged from the transferred assets. If set below
		/// requirements, the execution may fail and assets wouldn't be
		/// received.
		///
		/// `fee` is the amount to be spent to pay for execution in destination
		/// chain. Both fee and amount will be subtracted form the callers
		/// balance.
		///
		/// If `fee` is not high enough to cover for the execution costs in the
		/// destination chain, then the assets will be trapped in the
		/// destination chain
		///
		/// It's a no-op if any error on local XCM execution or message sending.
		/// Note sending assets out per se doesn't guarantee they would be
		/// received. Receiving depends on if the XCM message could be delivered
		/// by the network, and if the receiving chain would handle
		/// messages correctly.
		#[pallet::weight(Pallet::<T>::weight_of_transfer(currency_id.clone(), *amount, dest))]
		#[transactional]
		pub fn transfer_with_fee(
			origin: OriginFor<T>,
			currency_id: T::CurrencyId,
			amount: T::Balance,
			fee: T::Balance,
			dest: Box<VersionedMultiLocation>,
			dest_weight: Weight,
		) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let dest: MultiLocation = (*dest).try_into().map_err(|()| Error::<T>::BadVersion)?;

			Self::do_transfer_with_fee(who, currency_id, amount, fee, dest, dest_weight)
		}

		/// Transfer `MultiAsset` specifying the fee and amount as separate.
		///
		/// `dest_weight` is the weight for XCM execution on the dest chain, and
		/// it would be charged from the transferred assets. If set below
		/// requirements, the execution may fail and assets wouldn't be
		/// received.
		///
		/// `fee` is the multiasset to be spent to pay for execution in
		/// destination chain. Both fee and amount will be subtracted form the
		/// callers balance For now we only accept fee and asset having the same
		/// `MultiLocation` id.
		///
		/// If `fee` is not high enough to cover for the execution costs in the
		/// destination chain, then the assets will be trapped in the
		/// destination chain
		///
		/// It's a no-op if any error on local XCM execution or message sending.
		/// Note sending assets out per se doesn't guarantee they would be
		/// received. Receiving depends on if the XCM message could be delivered
		/// by the network, and if the receiving chain would handle
		/// messages correctly.
		#[pallet::weight(Pallet::<T>::weight_of_transfer_multiasset(asset, dest))]
		#[transactional]
		pub fn transfer_multiasset_with_fee(
			origin: OriginFor<T>,
			asset: Box<VersionedMultiAsset>,
			fee: Box<VersionedMultiAsset>,
			dest: Box<VersionedMultiLocation>,
			dest_weight: Weight,
		) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let asset: MultiAsset = (*asset).try_into().map_err(|()| Error::<T>::BadVersion)?;
			let fee: MultiAsset = (*fee).try_into().map_err(|()| Error::<T>::BadVersion)?;
			let dest: MultiLocation = (*dest).try_into().map_err(|()| Error::<T>::BadVersion)?;

			Self::do_transfer_multiasset_with_fee(who, asset, fee, dest, dest_weight)
		}

		/// Transfer several currencies specifying the item to be used as fee
		///
		/// `dest_weight` is the weight for XCM execution on the dest chain, and
		/// it would be charged from the transferred assets. If set below
		/// requirements, the execution may fail and assets wouldn't be
		/// received.
		///
		/// `fee_item` is index of the currencies tuple that we want to use for
		/// payment
		///
		/// It's a no-op if any error on local XCM execution or message sending.
		/// Note sending assets out per se doesn't guarantee they would be
		/// received. Receiving depends on if the XCM message could be delivered
		/// by the network, and if the receiving chain would handle
		/// messages correctly.
		#[pallet::weight(Pallet::<T>::weight_of_transfer_multicurrencies(currencies, fee_item, dest))]
		#[transactional]
		pub fn transfer_multicurrencies(
			origin: OriginFor<T>,
			currencies: Vec<(T::CurrencyId, T::Balance)>,
			fee_item: u32,
			dest: Box<VersionedMultiLocation>,
			dest_weight: Weight,
		) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let dest: MultiLocation = (*dest).try_into().map_err(|()| Error::<T>::BadVersion)?;

			Self::do_transfer_multicurrencies(who, currencies, fee_item, dest, dest_weight)
		}

		#[pallet::weight(Pallet::<T>::weight_of_send_transact())]
		#[transactional]
		pub fn transact(
			origin: OriginFor<T>,
			currency_id: T::CurrencyId,
			dest_id: u32,
			dest_weight: Weight,
			encoded_call_data: BoundedVec<u8, T::MaxTransactSize>,
			transact_fee: T::Balance,
		) -> DispatchResult {
			// TODO: make the limit u8 or hard-code the constant in the ORML code
			ensure!(T::MaxTransactSize::get() <= 256u32, Error::<T>::TransactTooLarge);

			let who = ensure_signed(origin)?;

			Self::do_transact(who, currency_id, transact_fee, dest_id, dest_weight, encoded_call_data)
		}

		#[pallet::weight(Pallet::<T>::weight_of_transfer_with_transact(currency_id.clone(), *amount, *dest_chain_id))]
		#[transactional]
		pub fn transfer_with_transact(
			origin: OriginFor<T>,
			currency_id: T::CurrencyId,
			amount: T::Balance,
			dest_chain_id: u32,
			dest_weight: Weight,
			encoded_call_data: BoundedVec<u8, T::MaxTransactSize>,
			transact_fee: T::Balance,
		) -> DispatchResult {
			// TODO: make the limit u8 or hard-code the constant in the ORML code
			ensure!(T::MaxTransactSize::get() <= 256u32, Error::<T>::TransactTooLarge);

			let who = ensure_signed(origin)?;

			Self::do_transfer_with_transact(
				who,
				currency_id,
				amount,
				dest_chain_id,
				dest_weight,
				encoded_call_data,
				transact_fee,
			)
		}

		/// Transfer several `MultiAsset` specifying the item to be used as fee
		///
		/// `dest_weight` is the weight for XCM execution on the dest chain, and
		/// it would be charged from the transferred assets. If set below
		/// requirements, the execution may fail and assets wouldn't be
		/// received.
		///
		/// `fee_item` is index of the MultiAssets that we want to use for
		/// payment
		///
		/// It's a no-op if any error on local XCM execution or message sending.
		/// Note sending assets out per se doesn't guarantee they would be
		/// received. Receiving depends on if the XCM message could be delivered
		/// by the network, and if the receiving chain would handle
		/// messages correctly.
		#[pallet::weight(Pallet::<T>::weight_of_transfer_multiassets(assets, fee_item, dest))]
		#[transactional]
		pub fn transfer_multiassets(
			origin: OriginFor<T>,
			assets: Box<VersionedMultiAssets>,
			fee_item: u32,
			dest: Box<VersionedMultiLocation>,
			dest_weight: Weight,
		) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let assets: MultiAssets = (*assets).try_into().map_err(|()| Error::<T>::BadVersion)?;
			let dest: MultiLocation = (*dest).try_into().map_err(|()| Error::<T>::BadVersion)?;

			// We first grab the fee
			let fee: &MultiAsset = assets.get(fee_item as usize).ok_or(Error::<T>::AssetIndexNonExistent)?;

			Self::do_transfer_multiassets(who, assets.clone(), fee.clone(), dest, dest_weight, None)
		}
	}

	impl<T: Config> Pallet<T> {
		fn do_transact(
			who: T::AccountId,
			transact_currency_id: T::CurrencyId,
			transact_fee_amount: T::Balance,
			dest_chain_id: u32,
			dest_weight: Weight,
			encoded_call_data: BoundedVec<u8, T::MaxTransactSize>,
		) -> DispatchResult {
			let transact_fee_location: MultiLocation = T::CurrencyIdConvert::convert(transact_currency_id)
				.ok_or(Error::<T>::NotCrossChainTransferableCurrency)?;

			let origin_location_interior = T::AccountIdToMultiLocation::convert(who).interior;
			let dest_chain_location: MultiLocation = (1, Parachain(dest_chain_id)).into();
			let refund_recipient = T::SelfLocation::get();
			Self::send_transact(
				transact_fee_location,
				transact_fee_amount,
				dest_chain_location,
				origin_location_interior,
				dest_weight,
				encoded_call_data,
				refund_recipient,
			)?;

			Ok(())
		}

		fn do_transfer_with_transact(
			who: T::AccountId,
			currency_id: T::CurrencyId,
			amount: T::Balance,
			dest_chain_id: u32,
			dest_weight: Weight,
			encoded_call_data: BoundedVec<u8, T::MaxTransactSize>,
			transact_fee_amount: T::Balance,
		) -> DispatchResult {
			ensure!(!amount.is_zero(), Error::<T>::ZeroAmount);

			let origin_location = T::AccountIdToMultiLocation::convert(who.clone());
			let mut dest_chain_location: MultiLocation = (1, Parachain(dest_chain_id)).into();
			let origin_location_interior = origin_location.clone().interior;
			// Need to append some interior to pass the `contains()` check and later the
			// `is_valid()` check in `do_transfer_multiassets()`. Only the chain part is
			// needed afterwards.
			let _ = dest_chain_location.append_with(origin_location_interior.clone());
			ensure!(
				T::MultiLocationsFilter::contains(&dest_chain_location),
				Error::<T>::NotSupportedMultiLocation
			);

			let transact_fee_location: MultiLocation =
				T::CurrencyIdConvert::convert(currency_id).ok_or(Error::<T>::NotCrossChainTransferableCurrency)?;
			let transfer_asset: MultiAsset = (transact_fee_location.clone(), amount.into()).into();
			let mut override_recipient = T::SelfLocation::get();
			let _ = override_recipient.append_with(origin_location_interior.clone());
			Self::do_transfer_multiassets(
				who,
				vec![transfer_asset.clone()].into(),
				transfer_asset,
				dest_chain_location.clone(),
				dest_weight,
				Some(override_recipient.clone()),
			)?;

			let dest_chain_location = dest_chain_location.chain_part().ok_or(Error::<T>::InvalidDest)?;
			Self::send_transact(
				transact_fee_location,
				transact_fee_amount,
				dest_chain_location,
				origin_location_interior,
				dest_weight,
				encoded_call_data,
				override_recipient,
			)?;

			Ok(())
		}

		fn send_transact(
			transact_fee_location: MultiLocation,
			transact_fee_amount: T::Balance,
			dest_chain_location: MultiLocation,
			origin_location_interior: Junctions,
			dest_weight: Weight,
			encoded_call_data: BoundedVec<u8, T::MaxTransactSize>,
			refund_recipient: MultiLocation,
		) -> DispatchResult {
			let ancestry = T::LocationInverter::ancestry();
			let mut transact_fee_asset: MultiAsset = (transact_fee_location, transact_fee_amount.into()).into();
			transact_fee_asset = transact_fee_asset
				.clone()
				.reanchored(&dest_chain_location, &ancestry)
				.map_err(|_| Error::<T>::CannotReanchor)?;

			let mut transact_fee_assets = MultiAssets::new();
			transact_fee_assets.push(transact_fee_asset.clone());
			let transact_fee_assets_len = transact_fee_assets.len();
			let instructions = vec![
				DescendOrigin(origin_location_interior),
				WithdrawAsset(transact_fee_assets),
				BuyExecution {
					fees: transact_fee_asset,
					weight_limit: WeightLimit::Limited(dest_weight),
				},
				Transact {
					// SovereignAccount of the user, not the chain
					origin_type: OriginKind::SovereignAccount,
					require_weight_at_most: dest_weight,
					call: encoded_call_data.into_inner().into(),
				},
				RefundSurplus,
				DepositAsset {
					assets: All.into(),
					max_assets: transact_fee_assets_len as u32,
					beneficiary: refund_recipient,
				},
			];

			T::XcmSender::send_xcm(dest_chain_location, Xcm(instructions)).map_err(|_| Error::<T>::SendFailure)?;

			Ok(())
		}

		fn do_transfer(
			who: T::AccountId,
			currency_id: T::CurrencyId,
			amount: T::Balance,
			dest: MultiLocation,
			dest_weight: Weight,
		) -> DispatchResult {
			let location: MultiLocation =
				T::CurrencyIdConvert::convert(currency_id).ok_or(Error::<T>::NotCrossChainTransferableCurrency)?;

			ensure!(!amount.is_zero(), Error::<T>::ZeroAmount);
			ensure!(
				T::MultiLocationsFilter::contains(&dest),
				Error::<T>::NotSupportedMultiLocation
			);

			let asset: MultiAsset = (location, amount.into()).into();
			Self::do_transfer_multiassets(who, vec![asset.clone()].into(), asset, dest, dest_weight, None)
		}

		fn do_transfer_with_fee(
			who: T::AccountId,
			currency_id: T::CurrencyId,
			amount: T::Balance,
			fee: T::Balance,
			dest: MultiLocation,
			dest_weight: Weight,
		) -> DispatchResult {
			let location: MultiLocation =
				T::CurrencyIdConvert::convert(currency_id).ok_or(Error::<T>::NotCrossChainTransferableCurrency)?;

			ensure!(!amount.is_zero(), Error::<T>::ZeroAmount);
			ensure!(!fee.is_zero(), Error::<T>::ZeroFee);
			ensure!(
				T::MultiLocationsFilter::contains(&dest),
				Error::<T>::NotSupportedMultiLocation
			);

			let asset = (location.clone(), amount.into()).into();
			let fee_asset: MultiAsset = (location, fee.into()).into();

			// Push contains saturated addition, so we should be able to use it safely
			let mut assets = MultiAssets::new();
			assets.push(asset);
			assets.push(fee_asset.clone());

			Self::do_transfer_multiassets(who, assets, fee_asset, dest, dest_weight, None)
		}

		fn do_transfer_multiasset(
			who: T::AccountId,
			asset: MultiAsset,
			dest: MultiLocation,
			dest_weight: Weight,
		) -> DispatchResult {
			Self::do_transfer_multiassets(who, vec![asset.clone()].into(), asset, dest, dest_weight, None)
		}

		fn do_transfer_multiasset_with_fee(
			who: T::AccountId,
			asset: MultiAsset,
			fee: MultiAsset,
			dest: MultiLocation,
			dest_weight: Weight,
		) -> DispatchResult {
			// Push contains saturated addition, so we should be able to use it safely
			let mut assets = MultiAssets::new();
			assets.push(asset);
			assets.push(fee.clone());

			Self::do_transfer_multiassets(who, assets, fee, dest, dest_weight, None)?;

			Ok(())
		}

		fn do_transfer_multicurrencies(
			who: T::AccountId,
			currencies: Vec<(T::CurrencyId, T::Balance)>,
			fee_item: u32,
			dest: MultiLocation,
			dest_weight: Weight,
		) -> DispatchResult {
			ensure!(
				currencies.len() <= T::MaxAssetsForTransfer::get(),
				Error::<T>::TooManyAssetsBeingSent
			);
			ensure!(
				T::MultiLocationsFilter::contains(&dest),
				Error::<T>::NotSupportedMultiLocation
			);

			let mut assets = MultiAssets::new();

			// Lets grab the fee amount and location first
			let (fee_currency_id, fee_amount) = currencies
				.get(fee_item as usize)
				.ok_or(Error::<T>::AssetIndexNonExistent)?;

			for (currency_id, amount) in &currencies {
				let location: MultiLocation = T::CurrencyIdConvert::convert(currency_id.clone())
					.ok_or(Error::<T>::NotCrossChainTransferableCurrency)?;
				ensure!(!amount.is_zero(), Error::<T>::ZeroAmount);

				// Push contains saturated addition, so we should be able to use it safely
				assets.push((location, (*amount).into()).into())
			}

			// We construct the fee now, since getting it from assets wont work as assets
			// sorts it
			let fee_location: MultiLocation = T::CurrencyIdConvert::convert(fee_currency_id.clone())
				.ok_or(Error::<T>::NotCrossChainTransferableCurrency)?;

			let fee: MultiAsset = (fee_location, (*fee_amount).into()).into();

			Self::do_transfer_multiassets(who, assets, fee, dest, dest_weight, None)
		}

		fn do_transfer_multiassets(
			who: T::AccountId,
			assets: MultiAssets,
			fee: MultiAsset,
			dest: MultiLocation,
			dest_weight: Weight,
			override_recipient: Option<MultiLocation>,
		) -> DispatchResult {
			ensure!(
				assets.len() <= T::MaxAssetsForTransfer::get(),
				Error::<T>::TooManyAssetsBeingSent
			);
			ensure!(
				T::MultiLocationsFilter::contains(&dest),
				Error::<T>::NotSupportedMultiLocation
			);
			let origin_location = T::AccountIdToMultiLocation::convert(who.clone());

			let mut non_fee_reserve: Option<MultiLocation> = None;
			let asset_len = assets.len();
			for i in 0..asset_len {
				let asset = assets.get(i).ok_or(Error::<T>::AssetIndexNonExistent)?;
				ensure!(
					matches!(asset.fun, Fungibility::Fungible(x) if !x.is_zero()),
					Error::<T>::InvalidAsset
				);
				// `assets` includes fee, the reserve location is decided by non fee asset
				if (fee != *asset && non_fee_reserve.is_none()) || asset_len == 1 {
					non_fee_reserve = T::ReserveProvider::reserve(asset);
				}
				// make sure all non fee assets share the same reserve
				if non_fee_reserve.is_some() {
					ensure!(
						non_fee_reserve == T::ReserveProvider::reserve(asset),
						Error::<T>::DistinctReserveForAssetAndFee
					);
				}
			}

			let fee_reserve = T::ReserveProvider::reserve(&fee);
			if fee_reserve != non_fee_reserve {
				// Current only support `ToReserve` with relay-chain asset as fee. other case
				// like `NonReserve` or `SelfReserve` with relay-chain fee is not support.
				ensure!(non_fee_reserve == dest.chain_part(), Error::<T>::InvalidAsset);

				let reserve_location = non_fee_reserve.clone().ok_or(Error::<T>::AssetHasNoReserve)?;
				let min_xcm_fee = T::MinXcmFee::get(&reserve_location).ok_or(Error::<T>::MinXcmFeeNotDefined)?;

				// min xcm fee should less than user fee
				let fee_to_dest: MultiAsset = (fee.id.clone(), min_xcm_fee).into();
				ensure!(fee_to_dest < fee, Error::<T>::FeeNotEnough);

				let mut assets_to_dest = MultiAssets::new();
				for i in 0..asset_len {
					let asset = assets.get(i).ok_or(Error::<T>::AssetIndexNonExistent)?;
					if fee != *asset {
						assets_to_dest.push(asset.clone());
					} else {
						assets_to_dest.push(fee_to_dest.clone());
					}
				}

				let mut assets_to_fee_reserve = MultiAssets::new();
				let asset_to_fee_reserve = subtract_fee(&fee, min_xcm_fee);
				assets_to_fee_reserve.push(asset_to_fee_reserve.clone());

				// First xcm sent to fee reserve chain and routed to dest chain.
				Self::execute_and_send_reserve_kind_xcm(
					origin_location.clone(),
					assets_to_fee_reserve,
					asset_to_fee_reserve,
					fee_reserve,
					&dest,
					Some(T::SelfLocation::get()),
					dest_weight,
				)?;

				// Second xcm send to dest chain.
				Self::execute_and_send_reserve_kind_xcm(
					origin_location,
					assets_to_dest,
					fee_to_dest,
					non_fee_reserve,
					&dest,
					None,
					dest_weight,
				)?;
			} else {
				Self::execute_and_send_reserve_kind_xcm(
					origin_location,
					assets.clone(),
					fee.clone(),
					non_fee_reserve,
					&dest,
					override_recipient,
					dest_weight,
				)?;
			}

			Self::deposit_event(Event::<T>::TransferredMultiAssets {
				sender: who,
				assets,
				fee,
				dest,
			});

			Ok(())
		}

		/// Execute and send xcm with given assets and fee to dest chain or
		/// reserve chain.
		fn execute_and_send_reserve_kind_xcm(
			origin_location: MultiLocation,
			assets: MultiAssets,
			fee: MultiAsset,
			reserve: Option<MultiLocation>,
			dest: &MultiLocation,
			maybe_recipient_override: Option<MultiLocation>,
			dest_weight: Weight,
		) -> DispatchResult {
			let (transfer_kind, dest, reserve, recipient) = Self::transfer_kind(reserve, dest)?;
			let recipient = match maybe_recipient_override {
				Some(recipient) => recipient,
				None => recipient,
			};

			let mut msg = match transfer_kind {
				SelfReserveAsset => Self::transfer_self_reserve_asset(assets, fee, dest, recipient, dest_weight)?,
				ToReserve => Self::transfer_to_reserve(assets, fee, dest, recipient, dest_weight)?,
				ToNonReserve => Self::transfer_to_non_reserve(assets, fee, reserve, dest, recipient, dest_weight)?,
			};

			let weight = T::Weigher::weight(&mut msg).map_err(|()| Error::<T>::UnweighableMessage)?;
			T::XcmExecutor::execute_xcm_in_credit(origin_location, msg, weight, weight)
				.ensure_complete()
				.map_err(|error| {
					log::error!("Failed execute transfer message with {:?}", error);
					Error::<T>::XcmExecutionFailed
				})?;

			Ok(())
		}

		fn transfer_self_reserve_asset(
			assets: MultiAssets,
			fee: MultiAsset,
			dest: MultiLocation,
			recipient: MultiLocation,
			dest_weight: Weight,
		) -> Result<Xcm<T::Call>, DispatchError> {
			Ok(Xcm(vec![TransferReserveAsset {
				assets: assets.clone(),
				dest: dest.clone(),
				xcm: Xcm(vec![
					Self::buy_execution(fee, &dest, dest_weight)?,
					Self::deposit_asset(recipient, assets.len() as u32),
				]),
			}]))
		}

		fn transfer_to_reserve(
			assets: MultiAssets,
			fee: MultiAsset,
			reserve: MultiLocation,
			recipient: MultiLocation,
			dest_weight: Weight,
		) -> Result<Xcm<T::Call>, DispatchError> {
			Ok(Xcm(vec![
				WithdrawAsset(assets.clone()),
				InitiateReserveWithdraw {
					assets: All.into(),
					reserve: reserve.clone(),
					xcm: Xcm(vec![
						Self::buy_execution(fee, &reserve, dest_weight)?,
						Self::deposit_asset(recipient, assets.len() as u32),
					]),
				},
			]))
		}

		fn transfer_to_non_reserve(
			assets: MultiAssets,
			fee: MultiAsset,
			reserve: MultiLocation,
			dest: MultiLocation,
			recipient: MultiLocation,
			dest_weight: Weight,
		) -> Result<Xcm<T::Call>, DispatchError> {
			let mut reanchored_dest = dest.clone();
			if reserve == MultiLocation::parent() {
				match dest {
					MultiLocation {
						parents,
						interior: X1(Parachain(id)),
					} if parents == 1 => {
						reanchored_dest = Parachain(id).into();
					}
					_ => {}
				}
			}

			Ok(Xcm(vec![
				WithdrawAsset(assets.clone()),
				InitiateReserveWithdraw {
					assets: All.into(),
					reserve: reserve.clone(),
					xcm: Xcm(vec![
						Self::buy_execution(half(&fee), &reserve, dest_weight)?,
						DepositReserveAsset {
							assets: All.into(),
							max_assets: assets.len() as u32,
							dest: reanchored_dest,
							xcm: Xcm(vec![
								Self::buy_execution(half(&fee), &dest, dest_weight)?,
								Self::deposit_asset(recipient, assets.len() as u32),
							]),
						},
					]),
				},
			]))
		}

		fn deposit_asset(recipient: MultiLocation, max_assets: u32) -> Instruction<()> {
			DepositAsset {
				assets: All.into(),
				max_assets,
				beneficiary: recipient,
			}
		}

		fn buy_execution(
			asset: MultiAsset,
			at: &MultiLocation,
			weight: Weight,
		) -> Result<Instruction<()>, DispatchError> {
			let ancestry = T::LocationInverter::ancestry();
			let fees = asset
				.reanchored(at, &ancestry)
				.map_err(|_| Error::<T>::CannotReanchor)?;
			Ok(BuyExecution {
				fees,
				weight_limit: WeightLimit::Limited(weight),
			})
		}

		/// Ensure has the `dest` has chain part and recipient part.
		fn ensure_valid_dest(dest: &MultiLocation) -> Result<(MultiLocation, MultiLocation), DispatchError> {
			if let (Some(dest), Some(recipient)) = (dest.chain_part(), dest.non_chain_part()) {
				Ok((dest, recipient))
			} else {
				Err(Error::<T>::InvalidDest.into())
			}
		}

		/// Get the transfer kind.
		///
		/// Returns `Err` if `dest` combination doesn't make sense, or `reserve`
		/// is none, else returns a tuple of:
		/// - `transfer_kind`.
		/// - asset's `reserve` parachain or relay chain location,
		/// - `dest` parachain or relay chain location.
		/// - `recipient` location.
		fn transfer_kind(
			reserve: Option<MultiLocation>,
			dest: &MultiLocation,
		) -> Result<(TransferKind, MultiLocation, MultiLocation, MultiLocation), DispatchError> {
			let (dest, recipient) = Self::ensure_valid_dest(dest)?;

			let self_location = T::SelfLocation::get();
			ensure!(dest != self_location, Error::<T>::NotCrossChainTransfer);
			let reserve = reserve.ok_or(Error::<T>::AssetHasNoReserve)?;
			let transfer_kind = if reserve == self_location {
				SelfReserveAsset
			} else if reserve == dest {
				ToReserve
			} else {
				ToNonReserve
			};
			Ok((transfer_kind, dest, reserve, recipient))
		}
	}

	// weights
	impl<T: Config> Pallet<T> {
		/// Returns weight of `transfer_multiasset` call.
		fn weight_of_transfer_multiasset(asset: &VersionedMultiAsset, dest: &VersionedMultiLocation) -> Weight {
			let asset: Result<MultiAsset, _> = asset.clone().try_into();
			let dest = dest.clone().try_into();
			if let (Ok(asset), Ok(dest)) = (asset, dest) {
				if let Ok((transfer_kind, dest, _, reserve)) =
					Self::transfer_kind(T::ReserveProvider::reserve(&asset), &dest)
				{
					let mut msg = match transfer_kind {
						SelfReserveAsset => Xcm(vec![TransferReserveAsset {
							assets: vec![].into(),
							dest,
							xcm: Xcm(vec![]),
						}]),
						ToReserve | ToNonReserve => Xcm(vec![
							WithdrawAsset(MultiAssets::from(asset)),
							InitiateReserveWithdraw {
								assets: All.into(),
								// `dest` is always (equal to) `reserve` in both cases
								reserve,
								xcm: Xcm(vec![]),
							},
						]),
					};
					return T::Weigher::weight(&mut msg)
						.map_or(Weight::max_value(), |w| T::BaseXcmWeight::get().saturating_add(w));
				}
			}
			0
		}

		/// Returns weight of `transfer` call.
		fn weight_of_transfer(currency_id: T::CurrencyId, amount: T::Balance, dest: &VersionedMultiLocation) -> Weight {
			if let Some(location) = T::CurrencyIdConvert::convert(currency_id) {
				let asset = (location, amount.into()).into();
				Self::weight_of_transfer_multiasset(&asset, dest)
			} else {
				0
			}
		}

		/// Returns weight of `send_transact` call.
		fn weight_of_send_transact() -> Weight {
			let mut msg = Xcm(vec![]);
			return T::Weigher::weight(&mut msg)
				.map_or(Weight::max_value(), |w| T::BaseXcmWeight::get().saturating_add(w));
		}

		/// Returns weight of `transfer_with_transact` call.
		fn weight_of_transfer_with_transact(
			currency_id: T::CurrencyId,
			amount: T::Balance,
			dest_chain_id: u32,
		) -> Weight {
			let dest_chain_location: MultiLocation = (1, Parachain(dest_chain_id)).into();
			if let Some(location) = T::CurrencyIdConvert::convert(currency_id) {
				let asset = (location, amount.into()).into();
				Self::weight_of_transfer_multiasset(&asset, &VersionedMultiLocation::V1(dest_chain_location))
					+ Self::weight_of_send_transact()
			} else {
				0
			}
		}

		/// Returns weight of `transfer` call.
		fn weight_of_transfer_multicurrencies(
			currencies: &[(T::CurrencyId, T::Balance)],
			fee_item: &u32,
			dest: &VersionedMultiLocation,
		) -> Weight {
			let mut assets: Vec<MultiAsset> = Vec::new();
			for (currency_id, amount) in currencies {
				if let Some(location) = T::CurrencyIdConvert::convert(currency_id.clone()) {
					let asset: MultiAsset = (location.clone(), (*amount).into()).into();
					assets.push(asset);
				} else {
					return 0;
				}
			}

			Self::weight_of_transfer_multiassets(&VersionedMultiAssets::from(MultiAssets::from(assets)), fee_item, dest)
		}

		/// Returns weight of `transfer_multiassets` call.
		fn weight_of_transfer_multiassets(
			assets: &VersionedMultiAssets,
			fee_item: &u32,
			dest: &VersionedMultiLocation,
		) -> Weight {
			let assets: Result<MultiAssets, ()> = assets.clone().try_into();
			let dest = dest.clone().try_into();
			if let (Ok(assets), Ok(dest)) = (assets, dest) {
				let reserve_location = Self::get_reserve_location(&assets, fee_item);
				if let Ok((transfer_kind, dest, _, reserve)) = Self::transfer_kind(reserve_location, &dest) {
					let mut msg = match transfer_kind {
						SelfReserveAsset => Xcm(vec![TransferReserveAsset {
							assets,
							dest,
							xcm: Xcm(vec![]),
						}]),
						ToReserve | ToNonReserve => Xcm(vec![
							WithdrawAsset(assets),
							InitiateReserveWithdraw {
								assets: All.into(),
								// `dest` is always (equal to) `reserve` in both cases
								reserve,
								xcm: Xcm(vec![]),
							},
						]),
					};
					return T::Weigher::weight(&mut msg)
						.map_or(Weight::max_value(), |w| T::BaseXcmWeight::get().saturating_add(w));
				}
			}
			0
		}

		/// Get reserve location by `assets` and `fee_item`. the `assets`
		/// includes fee asset and non fee asset. make sure assets have ge one
		/// asset. all non fee asset should share same reserve location.
		fn get_reserve_location(assets: &MultiAssets, fee_item: &u32) -> Option<MultiLocation> {
			let reserve_idx = if assets.len() == 1 {
				0
			} else if *fee_item == 0 {
				1
			} else {
				0
			};
			let asset = assets.get(reserve_idx);
			asset.and_then(T::ReserveProvider::reserve)
		}
	}

	impl<T: Config> XcmTransfer<T::AccountId, T::Balance, T::CurrencyId> for Pallet<T> {
		#[require_transactional]
		fn transfer(
			who: T::AccountId,
			currency_id: T::CurrencyId,
			amount: T::Balance,
			dest: MultiLocation,
			dest_weight: Weight,
		) -> DispatchResult {
			Self::do_transfer(who, currency_id, amount, dest, dest_weight)
		}

		#[require_transactional]
		fn transfer_multi_asset(
			who: T::AccountId,
			asset: MultiAsset,
			dest: MultiLocation,
			dest_weight: Weight,
		) -> DispatchResult {
			Self::do_transfer_multiasset(who, asset, dest, dest_weight)
		}
	}
}

/// Returns amount if `asset` is fungible, or zero.
fn fungible_amount(asset: &MultiAsset) -> u128 {
	if let Fungible(amount) = &asset.fun {
		*amount
	} else {
		Zero::zero()
	}
}

fn half(asset: &MultiAsset) -> MultiAsset {
	let half_amount = fungible_amount(asset)
		.checked_div(2)
		.expect("div 2 can't overflow; qed");
	MultiAsset {
		fun: Fungible(half_amount),
		id: asset.id.clone(),
	}
}

fn subtract_fee(asset: &MultiAsset, amount: u128) -> MultiAsset {
	let final_amount = fungible_amount(asset).checked_sub(amount).expect("fee too low; qed");
	MultiAsset {
		fun: Fungible(final_amount),
		id: asset.id.clone(),
	}
}
