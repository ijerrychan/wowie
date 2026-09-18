pub mod app;
pub mod control_panel;
pub mod steam;

pub mod dynamic {
    include!(concat!(env!("OUT_DIR"), "/dynamic_providers.rs"));
}
