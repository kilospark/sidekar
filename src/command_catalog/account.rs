use super::{CommandGroup, CommandSpec, spec};

pub const COMMANDS: &[CommandSpec] = &[
    spec(
        "device",
        "<login|logout|list>",
        "Device authentication and management",
        CommandGroup::Account,
        false,
        false,
        false,
    ),
    spec(
        "session",
        "<list>",
        "List active sessions for your account",
        CommandGroup::Account,
        false,
        false,
        false,
    ),
    spec(
        "totp",
        "<subcommand>",
        "Manage stored TOTP secrets",
        CommandGroup::Account,
        false,
        false,
        false,
    ),
    spec(
        "kv",
        "<set|get|list|delete|tag|history|rollback|exec>",
        "Encrypted KV store with tags, versioning, and secret exec",
        CommandGroup::Account,
        false,
        false,
        false,
    ),
];
