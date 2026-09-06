//! Defines the actor:entity/handler interface and exports it for actors to implement.
#[allow(warnings)]
mod bindings;

use bindings::exports::actor::entity::handler::Guest;

struct Component;

impl Guest for Component {
    fn on_spawn(_id: String, _state: Vec<u8>) -> Result<(), String> {
        Ok(())
    }

    fn on_message(_message: Vec<u8>) -> Result<(), String> {
        Ok(())
    }

    fn on_snapshot() -> Vec<u8> {
        Vec::new()
    }
}

bindings::export!(Component with_types_in bindings);
