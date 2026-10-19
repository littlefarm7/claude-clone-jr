//! Built-in tools for Jarvis.

pub mod custom;
pub mod delegate;
pub mod docs;
pub mod fs;
pub mod mcp;
pub mod media;
pub mod memory_tools;
pub mod ooxml;
pub mod pdfgen;
pub mod runtime;
pub mod shell;
pub mod skills;
pub mod testutil;
pub mod voice;
pub mod web;

use jarvis_core::config::Config;
use jarvis_core::tool::ToolRegistry;

/// All built-in tools (MCP and custom tools are added separately).
pub fn builtin_registry(_cfg: &Config) -> ToolRegistry {
    let mut r = ToolRegistry::new();
    fs::register(&mut r);
    shell::register(&mut r);
    memory_tools::register(&mut r);
    delegate::register(&mut r);
    web::register(&mut r);
    docs::register(&mut r);
    media::register(&mut r);
    voice::register(&mut r);
    r
}
