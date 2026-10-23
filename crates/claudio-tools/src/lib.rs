//! Built-in tools for Claudio.

pub mod api;
pub mod browse;
pub mod custom;
pub mod delegate;
pub mod docs;
pub mod downloads;
pub mod elevate;
pub mod fs;
pub mod mcp;
pub mod media;
pub mod memory_tools;
pub mod ooxml;
pub mod pdfgen;
pub mod runtime;
pub mod setup;
pub mod shell;
pub mod skills;
pub mod speech;
pub mod testutil;
pub mod voice;
pub mod web;

use claudio_core::config::Config;
use claudio_core::tool::ToolRegistry;

/// All built-in tools (MCP and custom tools are added separately).
pub fn builtin_registry(_cfg: &Config) -> ToolRegistry {
    let mut r = ToolRegistry::new();
    fs::register(&mut r);
    shell::register(&mut r);
    elevate::register(&mut r);
    memory_tools::register(&mut r);
    delegate::register(&mut r);
    web::register(&mut r);
    api::register(&mut r);
    browse::register(&mut r);
    docs::register(&mut r);
    media::register(&mut r);
    voice::register(&mut r);
    r
}
