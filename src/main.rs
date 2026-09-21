#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod core;
mod details;
mod platform;
mod plugin;
mod providers;
mod services;
mod ui;
mod utils;

pub(crate) use core::{classifier, page, provider, ranking, result, search};
pub(crate) use services::{icon_service, launcher};

fn main() -> anyhow::Result<()> {
    ui::run()
}
