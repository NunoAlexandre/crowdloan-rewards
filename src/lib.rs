// Copyright 2019-2020 PureStake Inc.
// This file is part of Moonbeam.

// Moonbeam is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Moonbeam is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Moonbeam.  If not, see <http://www.gnu.org/licenses/>.

//! # Crowdloan Rewards Pallet
//!
//! This pallet issues rewards to citizens who participated in a crowdloan on the backing relay
//! chain (eg Kusama) in order to help this parrachain acquire a parachain slot.
//!
//! ## Monetary Policy
//!
//! This is simple and mock for now. We can do whatever we want.
//! This pallet stores a constant  "reward ratio" which is the number of reward tokens to pay per
//! contributed token. In simple cases this can be 1, but needs to be customizeable to allow for
//! vastly differing absolute token supplies between relay and para.
//! Vesting is also linear. No tokens are vested at genesis and they unlock linearly until a
//! predecided block number. Vesting computations happen on demand when payouts are requested. So
//! no block weight is ever wasted on this, and there is no "base-line" cost of updating vestings.
//! Like I said, we can anything we want there. Even a non-linear reward curve to disincentivize
//! whales.
//!
//! ## Payout Mechanism
//!
//! The current payout mechanism requires contributors to claim their payouts. Because they are
//! paying the transaction fees for this themselves, they can do it as often as every block, or
//! wait and claim the entire thing once it is fully vested. We could consider auto payouts if we
//! want.
//!
//! ## Sourcing Contribution Information
//!
//! The pallet can learn about the crowdloan contributions in several ways.
//!
//! * **Assocaited at Genesis**
//!
//! The simplest way is that the native identity and contribution amount are configured at genesis.
//! This makes sense in a scenario wherethe crowdloan took place entirely offchain.
//!
//! * **Unassociated at Genesis**
//!
//! When the crowdloan takes place on-relay-chain, contributors will not have a way to specify a native account
//! into which they will receive rewards on the parachain. TODO that would be easy to add to the
//! relay chain actually. In this case the genesis config contains information about the
//! relay chain style contributor address, and the contribution amount. In this case the
//! contributor is responsible for making a transaction that associates a native ID. The tx
//! includes a signature by the relay chain idetity over the native identity.
//!
//! * **ReadingRelayState**
//!
//! The most elegant, but most complex solution would be for the para to read the contributions
//! directly from the relay state. Blocked by https://github.com/paritytech/cumulus/issues/320 so
//! I won't persue it further right now. I can't decide whether that would really add security /
//! trustlessness, or is just a sexy blockchain thing to do. Contributors can always audit the
//! genesis block and make sure their contribution is in it, so in that sense reading relay state
//! isn't necessary. But if a single contribution is left out, the rest of the contributors might
//! not care enough to delay network launch. The little guy might get sensored.

#![cfg_attr(not(feature = "std"), no_std)]

use frame_support::pallet;
pub use pallet::*;

#[pallet]
pub mod pallet {

	use frame_support::dispatch::fmt::Debug;
	use frame_support::pallet_prelude::*;
	use frame_support::traits::Currency;
	use frame_support::traits::Vec;
	use frame_system::pallet_prelude::*;
	use log::warn;
	use sp_core::crypto::AccountId32;
	use sp_runtime::traits::Verify;
	use sp_runtime::{MultiSignature, SaturatedConversion};
	use std::convert::TryInto;

	/// The Author Filter pallet
	#[pallet::pallet]
	pub struct Pallet<T>(PhantomData<T>);

	/// Configuration trait of this pallet.
	#[pallet::config]
	pub trait Config: frame_system::Config {
		/// The overarching event type
		type Event: From<Event<Self>> + IsType<<Self as frame_system::Config>::Event>;
		/// The currency in which the rewards will be paid (probably the parachain native currency)
		type RewardCurrency: Currency<Self::AccountId>;

		// TODO What trait bounds do I need here? I think concretely we would
		// be using MultiSigner? Or maybe MultiAccount? I copied these from frame_system
		/// The AccountId type contributors used on the relay chain.
		type RelayChainAccountId: Parameter
			+ Member
			+ MaybeSerializeDeserialize
			+ Ord
			+ Default
			+ Debug
			+ Into<AccountId32>;

		/// The total vesting period.
		type VestingPeriod: Get<Self::BlockNumber>;
	}

	type BalanceOf<T> = <<T as Config>::RewardCurrency as Currency<
		<T as frame_system::Config>::AccountId,
	>>::Balance;
	/// Stores info about the rewards owed as well as how much has been vested so far.
	/// For a primer on this kind of design, see the recipe on compounding interest
	/// https://substrate.dev/recipes/fixed-point.html#continuously-compounding
	#[derive(Default, Clone, Encode, Decode, RuntimeDebug)]
	pub struct RewardInfo<T: Config> {
		pub total_reward: BalanceOf<T>,
		pub last_paid: T::BlockNumber,
	}

	// No hooks
	#[pallet::hooks]
	impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {}

	#[pallet::call]
	impl<T: Config> Pallet<T> {
		/// Associate a native rewards_destination identity with a crowdloan contribution.
		///
		/// This is an unsigned call because the caller may not have any funds to pay fees with.
		/// This is inspired by Polkadot's claims pallet:
		/// https://github.com/paritytech/polkadot/blob/master/runtime/common/src/claims.rs
		///
		/// This function and the entire concept of unassociated contributions may be obviated if
		/// They will accept a memo filed in the Polkadot crowdloan pallet.
		#[pallet::weight(0)]
		pub fn associate_native_identity(
			origin: OriginFor<T>,
			reward_account: T::AccountId,
			relay_account: T::RelayChainAccountId,
			proof: MultiSignature,
		) -> DispatchResultWithPostInfo {
			ensure_none(origin)?;
			//TODO check the proof:
			// 1. Is signed by an actual unassociated contributor
			// 2. Signs a valid native identity
			// Check the proof. The Proof consists of a Signature of the rewarded account with the
			// claimer key
			let payload = reward_account.encode();
			ensure!(
				proof.verify(payload.as_slice(), &relay_account.clone().into()),
				Error::<T>::InvalidClaimSignature
			);

			// We ensure the mapping does not exist yet to avoid multi-claiming
			ensure!(
				AccountsMapping::<T>::get(&relay_account).is_none(),
				Error::<T>::AlreadyAssociated
			);

			// Upon error this should check the relay chain state in this case
			let reward_info = UnassociatedContributions::<T>::get(&relay_account)
				.ok_or(Error::<T>::NoAssociatedClaim)?;

			// Insert on payable
			AccountsPayable::<T>::insert(&reward_account, &reward_info);

			// Remove from unassociated
			<UnassociatedContributions<T>>::remove(&relay_account);

			// Insert in mapping
			AccountsMapping::<T>::insert(&relay_account, &reward_account);

			// Emit Event
			Self::deposit_event(Event::NativeIdentityAssociated(
				relay_account,
				reward_account,
				reward_info.total_reward,
			));

			Ok(Default::default())
		}

		/// Collect whatever portion of your reward are currently vested.
		#[pallet::weight(0)]
		pub fn show_me_the_money(origin: OriginFor<T>) -> DispatchResultWithPostInfo {
			let payee = ensure_signed(origin)?;

			// Calculate the veted amount on demand.
			let mut info =
				AccountsPayable::<T>::get(&payee).ok_or(Error::<T>::NoAssociatedClaim)?;
			let now = frame_system::Module::<T>::block_number();
			//TODO This part doesn't compile because of a million stupid errors about converting
			// between u32, Balance, and BlockNumber. I think that is solvable, just annoying.
			let payable_per_block = info.total_reward
				/ T::VestingPeriod::get()
					.saturated_into::<u128>()
					.try_into()
					.ok()
					.ok_or(Error::<T>::WrongConversionU128ToBalance)?; //TODO safe math;
			let payable_period = T::VestingPeriod::get() - info.last_paid;
			let pay_period_as_balance: BalanceOf<T> = payable_period
				.saturated_into::<u128>()
				.try_into()
				.ok()
				.ok_or(Error::<T>::WrongConversionU128ToBalance)?;
			let payable_amount = pay_period_as_balance * payable_per_block;

			// Update the stored info
			info.last_paid = now;
			AccountsPayable::<T>::insert(&payee, &info);

			// Make the payment
			// TODO where are these reward funds coming from? Currently I'm just minting them right here.
			// 1. We could have an associated type to absorb the imbalance.
			// 2. We could have this pallet control a pot of funds, and initialize it at genesis.
			T::RewardCurrency::deposit_creating(&payee, payable_amount);

			// Emit event
			Self::deposit_event(Event::RewardsPaid(payee, payable_amount));

			Ok(Default::default())
		}
	}

	#[pallet::error]
	pub enum Error<T> {
		/// User trying to claim an award did not have an claim associated with it. This may mean
		/// they did not contribute to the crowdloan, or they have not yet associated a native id
		/// with their contribution
		AlreadyAssociated,
		InvalidClaimSignature,
		NoAssociatedClaim,
		WrongConversionU128ToBalance,
	}

	#[pallet::storage]
	pub type AccountsPayable<T: Config> =
		StorageMap<_, Blake2_128Concat, T::AccountId, RewardInfo<T>>;
	#[pallet::storage]
	pub type AccountsMapping<T: Config> =
		StorageMap<_, Blake2_128Concat, T::RelayChainAccountId, T::AccountId>;
	#[pallet::storage]
	pub type UnassociatedContributions<T: Config> =
		StorageMap<_, Blake2_128Concat, T::RelayChainAccountId, RewardInfo<T>>;

	// Design decision:
	// Genesis config contributions are specified in relay-chain currency
	// Conversion to reward currency happens when constructing genesis
	// This pallets storages are all in terms of reward currency
	#[pallet::genesis_config]
	pub struct GenesisConfig<T: Config> {
		/// Contributions that have a native account id associated already.
		pub associated: Vec<(T::RelayChainAccountId, T::AccountId, u32)>,
		/// Contributions that will need a native account id to be associated through an extrinsic.
		pub unassociated: Vec<(T::RelayChainAccountId, u32)>,
		/// The ratio of (reward tokens to be paid) / (relay chain funds contributed)
		/// This is dead stupid simple using a u32. So the reward amount has to be an integer
		/// multiple of the contribution amount. A better fixed-ratio solution would be
		/// https://crates.parity.io/sp_arithmetic/fixed_point/struct.FixedU128.html
		/// We could also do something fancy and non-linear if the need arises.
		pub reward_ratio: u32,
	}

	#[cfg(feature = "std")]
	impl<T: Config> Default for GenesisConfig<T> {
		fn default() -> Self {
			Self {
				associated: Vec::new(),
				unassociated: Vec::new(),
				reward_ratio: 1,
			}
		}
	}

	#[pallet::genesis_build]
	impl<T: Config> GenesisBuild<T> for GenesisConfig<T> {
		fn build(&self) {
			// Warn if no contributions (associated or not) are specified
			if self.associated.is_empty() && self.unassociated.is_empty() {
				warn!("Rewards: No contributions configured. Pallet will not be useable.")
			}

			// Initialize storage for associated contributions
			self.associated
				.iter()
				.for_each(|(relay_account, native_account, contrib)| {
					let reward_info = RewardInfo {
						total_reward: BalanceOf::<T>::from(*contrib)
							* BalanceOf::<T>::from(self.reward_ratio), //TODO safe math?
						last_paid: 0u32.into(),
					};
					AccountsPayable::<T>::insert(native_account, reward_info);
					AccountsMapping::<T>::insert(relay_account, native_account);
				});

			// Initialize storage for UN-associated contributions
			self.unassociated
				.iter()
				.for_each(|(relay_account, contrib)| {
					//TODO: 📠🍝
					let reward_info = RewardInfo {
						total_reward: BalanceOf::<T>::from(*contrib)
							* BalanceOf::<T>::from(self.reward_ratio), //TODO safe math?
						last_paid: 0u32.into(),
					};
					UnassociatedContributions::<T>::insert(relay_account, reward_info);
				});
		}
	}

	#[pallet::event]
	#[pallet::generate_deposit(fn deposit_event)]
	pub enum Event<T: Config> {
		/// Someone has proven they made a contribution and associated a native identity with it.
		/// Data is the relay account,  native account and the total amount of _rewards_ that will be paid
		NativeIdentityAssociated(T::RelayChainAccountId, T::AccountId, BalanceOf<T>),
		/// A contributor has claimed some rewards.
		/// Data is the account getting paid and the amount of rewards paid.
		RewardsPaid(T::AccountId, BalanceOf<T>),
	}
}
