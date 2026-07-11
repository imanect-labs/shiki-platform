//! HTTP ルートハンドラ群。

pub mod admin;
pub mod app_platform;
pub mod artifacts;
pub mod auth;
pub mod chat;
pub mod chat_approval;
pub mod chat_dto;
pub mod collab;
pub mod data;
pub mod data_fsm;
pub mod data_records;
pub mod data_views;
pub mod directory;
pub mod files;
pub mod folders;
pub mod me;
pub mod mini_apps;
pub mod search;
pub mod secrets;
pub mod shares;
pub mod skills;
pub mod ui_actions;
pub mod ui_specs;
pub mod workflows;

pub use me::get_me;
