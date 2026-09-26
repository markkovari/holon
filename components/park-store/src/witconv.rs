//! The contract's records, between this crate's generated bindings and
//! `holon_park::model` (the serde types the wire is made of) — both
//! directions.
//!
//! Shared by `park-store` and `park-gateway` as a SOURCE file (`#[path]`), not
//! a library — see `components/vcs-store/src/witconv.rs`'s own doc for why:
//! each crate's `bindings.rs` makes its own Rust types for the same WIT types
//! (ADR-0095), so this is compiled once per crate against `crate::wit::t`
//! (the `holon:park/types` bindings), which each crate points at its own.

use holon_park::model as m;
use holon_park::ParkError;

use crate::wit::t;

pub trait ToWit {
    type Out;
    fn to_wit(self) -> Self::Out;
}

pub trait ToModel {
    type Out;
    fn to_model(self) -> Self::Out;
}

macro_rules! identity {
    ($($ty:ty),*) => {$(
        impl ToWit for $ty { type Out = $ty; fn to_wit(self) -> $ty { self } }
        impl ToModel for $ty { type Out = $ty; fn to_model(self) -> $ty { self } }
    )*};
}
identity!(String, u64, u32, u8, bool, ());

impl<T: ToWit> ToWit for Option<T> {
    type Out = Option<T::Out>;
    fn to_wit(self) -> Self::Out {
        self.map(ToWit::to_wit)
    }
}
impl<T: ToModel> ToModel for Option<T> {
    type Out = Option<T::Out>;
    fn to_model(self) -> Self::Out {
        self.map(ToModel::to_model)
    }
}
impl<T: ToWit> ToWit for Vec<T> {
    type Out = Vec<T::Out>;
    fn to_wit(self) -> Self::Out {
        self.into_iter().map(ToWit::to_wit).collect()
    }
}
impl<T: ToModel> ToModel for Vec<T> {
    type Out = Vec<T::Out>;
    fn to_model(self) -> Self::Out {
        self.into_iter().map(ToModel::to_model).collect()
    }
}

/// A record with the same name and fields on both sides.
macro_rules! record {
    ($name:ident { $($field:ident),* $(,)? }) => {
        impl ToWit for m::$name {
            type Out = t::$name;
            fn to_wit(self) -> t::$name { t::$name { $($field: self.$field.to_wit()),* } }
        }
        impl ToModel for t::$name {
            type Out = m::$name;
            fn to_model(self) -> m::$name { m::$name { $($field: self.$field.to_model()),* } }
        }
    };
}

/// A variant or enum with the same name and cases on both sides; a case has
/// no payload or one.
macro_rules! variant {
    ($name:ident { $($case:ident $(($x:ident))?),* $(,)? }) => {
        impl ToWit for m::$name {
            type Out = t::$name;
            fn to_wit(self) -> t::$name {
                match self { $(m::$name::$case $(($x))? => t::$name::$case $(($x.to_wit()))?),* }
            }
        }
        impl ToModel for t::$name {
            type Out = m::$name;
            fn to_model(self) -> m::$name {
                match self { $(t::$name::$case $(($x))? => m::$name::$case $(($x.to_model()))?),* }
            }
        }
    };
}

record!(Agent { id, goal, model });
record!(PollSpec { url, interval_secs });
record!(OutboundCall { correlation, description, deadline, poll });
record!(CallResult { ok, body, detail });
variant!(ParkOutcome { Parked, AlreadyParked, AlreadyWoken });
record!(ParkResult { ticket, outcome });
variant!(TurnStatus { Parked, Ready, Resumed, Cancelled, Expired });
record!(TicketEntry { ticket, session, status, parked_at, woken_at, resumed_at });

impl ToWit for ParkError {
    type Out = t::ParkError;
    fn to_wit(self) -> t::ParkError {
        match self {
            ParkError::NotFound(s) => t::ParkError::NotFound(s),
            ParkError::AlreadyClosed(s) => t::ParkError::AlreadyClosed(s),
            ParkError::Storage(s) => t::ParkError::StorageError(s),
            ParkError::Invalid(s) => t::ParkError::Invalid(s),
        }
    }
}

impl ToModel for t::ParkError {
    type Out = ParkError;
    fn to_model(self) -> ParkError {
        match self {
            t::ParkError::NotFound(s) => ParkError::NotFound(s),
            t::ParkError::AlreadyClosed(s) => ParkError::AlreadyClosed(s),
            t::ParkError::StorageError(s) => ParkError::Storage(s),
            t::ParkError::Invalid(s) => ParkError::Invalid(s),
        }
    }
}
