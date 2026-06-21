use super::*;

/// 100 short, memorable nicknames for agents.
const NICKNAMES: &[&str] = &[
    "badger",
    "bantam",
    "barbet",
    "basilisk",
    "bison",
    "bobcat",
    "bonobo",
    "borzoi",
    "caiman",
    "capybara",
    "caracal",
    "cassowary",
    "cheetah",
    "chinchilla",
    "cicada",
    "civet",
    "coati",
    "condor",
    "corgi",
    "cougar",
    "coyote",
    "crane",
    "cuckoo",
    "curlew",
    "dingo",
    "dormouse",
    "drongo",
    "dugong",
    "dunlin",
    "egret",
    "ermine",
    "falcon",
    "fennec",
    "ferret",
    "finch",
    "flamingo",
    "flounder",
    "gannet",
    "gazelle",
    "gecko",
    "gerbil",
    "gibbon",
    "gopher",
    "grouse",
    "guppy",
    "harrier",
    "hedgehog",
    "heron",
    "hoopoe",
    "hornet",
    "husky",
    "hyena",
    "ibis",
    "iguana",
    "impala",
    "jackal",
    "jackdaw",
    "jaguar",
    "jerboa",
    "kakapo",
    "kestrel",
    "kinkajou",
    "kiwi",
    "kodiak",
    "komodo",
    "lemur",
    "leopard",
    "limpet",
    "loris",
    "macaw",
    "mako",
    "mamba",
    "mandrill",
    "mantis",
    "margay",
    "marlin",
    "marmot",
    "merlin",
    "mink",
    "mongoose",
    "moray",
    "narwhal",
    "newt",
    "numbat",
    "ocelot",
    "okapi",
    "oriole",
    "osprey",
    "otter",
    "pangolin",
    "parrot",
    "pelican",
    "penguin",
    "peregrine",
    "pika",
    "piranha",
    "platypus",
    "quail",
    "quetzal",
    "quokka",
    "raven",
    "robin",
    "rooster",
    "sable",
    "salmon",
    "scarab",
    "serval",
    "shrike",
    "sparrow",
    "starling",
    "stoat",
    "taipan",
    "tamarin",
    "tanager",
    "tarpon",
    "tenrec",
    "tern",
    "thrush",
    "toucan",
    "uakari",
    "umbrellabird",
    "viper",
    "vizsla",
    "vulture",
    "wallaby",
    "walrus",
    "weasel",
    "whippet",
    "wombat",
    "woodpecker",
    "xerus",
    "yak",
    "zebu",
    "zorilla",
];

/// Pick a nickname for a project, trying to reuse previous if available.
pub fn pick_nickname_standalone() -> String {
    pick_nickname_for_project(None)
}

fn pid_from_agent_pane(pane: &str) -> Option<i32> {
    for prefix in ["pty-", "repl-", "cli-"] {
        if let Some(pid_str) = pane.strip_prefix(prefix)
            && let Ok(pid) = pid_str.parse::<i32>()
        {
            return Some(pid);
        }
    }
    None
}

fn pid_alive(pid: i32) -> bool {
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn agent_blocks_nickname_reuse(agent: &BrokerAgent) -> bool {
    match agent.id.pane.as_deref().and_then(pid_from_agent_pane) {
        Some(pid) => pid_alive(pid),
        None => true,
    }
}

fn live_used_nicknames() -> HashSet<String> {
    let mut used = HashSet::new();
    if let Ok(agents) = broker::list_agents(None) {
        for agent in agents {
            if !agent_blocks_nickname_reuse(&agent) {
                continue;
            }
            if let Some(nick) = agent.id.nick {
                used.insert(nick);
            }
        }
    }
    used
}

fn pick_available_nickname(used: &HashSet<String>) -> String {
    use rand::seq::SliceRandom;

    let mut available: Vec<&str> = NICKNAMES
        .iter()
        .filter(|n| !used.contains(**n))
        .copied()
        .collect();
    available.shuffle(&mut rand::rng());
    available.first().map(|s| s.to_string()).unwrap_or_else(|| {
        for _ in 0..16 {
            let r: u16 = rand::random();
            let nick = format!("agent-{:04x}", r);
            if !used.contains(&nick) {
                return nick;
            }
        }
        let r: u32 = rand::random();
        format!("agent-{:08x}", r)
    })
}

pub fn pick_nickname_for_project(project: Option<&str>) -> String {
    let used = live_used_nicknames();

    if let Some(proj) = project {
        let nick_key = format!("_nick:{}", proj);
        let stored = broker::kv_get(&nick_key).ok().flatten().map(|e| e.value);

        if let Some(nick) = stored {
            if !used.contains(&nick) {
                return nick;
            }
            return pick_available_nickname(&used);
        }

        let picked = pick_available_nickname(&used);

        let _ = broker::kv_set(&nick_key, &picked, None);
        picked
    } else {
        pick_available_nickname(&used)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    fn with_temp_home<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = crate::test_home_lock()
            .lock()
            .map_err(|_| anyhow!("failed to lock test HOME mutex"))?;
        let old_home = env::var_os("HOME");
        let temp_home =
            env::temp_dir().join(format!("sidekar-nick-test-{}", rand::random::<u64>()));
        fs::create_dir_all(&temp_home)?;
        unsafe { env::set_var("HOME", &temp_home) };
        let result = f();
        match old_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
        let _ = fs::remove_dir_all(&temp_home);
        result
    }

    fn register_test_agent(name: &str, nick: &str, pane: &str) -> Result<()> {
        let identity = AgentId {
            name: name.to_string(),
            nick: Some(nick.to_string()),
            session: Some("test-session".to_string()),
            pane: Some(pane.to_string()),
            agent_type: Some("sidekar-test".to_string()),
        };
        broker::register_agent(&identity, Some(pane))
    }

    #[test]
    fn reuses_stored_project_nickname() -> Result<()> {
        with_temp_home(|| {
            broker::init_db()?;
            broker::kv_set("_nick:/tmp/project", "borzoi", None)?;

            assert_eq!(pick_nickname_for_project(Some("/tmp/project")), "borzoi");
            Ok(())
        })
    }

    #[test]
    fn stale_local_agent_does_not_block_stored_project_nickname() -> Result<()> {
        with_temp_home(|| {
            broker::init_db()?;
            broker::kv_set("_nick:/tmp/project", "borzoi", None)?;
            register_test_agent("old-agent", "borzoi", "pty-99999999")?;

            assert_eq!(pick_nickname_for_project(Some("/tmp/project")), "borzoi");
            assert_eq!(
                broker::kv_get("_nick:/tmp/project")?.map(|entry| entry.value),
                Some("borzoi".to_string())
            );
            Ok(())
        })
    }

    #[test]
    fn live_conflict_gets_temporary_nickname_without_overwriting_project_mapping() -> Result<()> {
        with_temp_home(|| {
            broker::init_db()?;
            broker::kv_set("_nick:/tmp/project", "borzoi", None)?;
            let pane = format!("pty-{}", std::process::id());
            register_test_agent("live-agent", "borzoi", &pane)?;

            let picked = pick_nickname_for_project(Some("/tmp/project"));

            assert_ne!(picked, "borzoi");
            assert_eq!(
                broker::kv_get("_nick:/tmp/project")?.map(|entry| entry.value),
                Some("borzoi".to_string())
            );
            Ok(())
        })
    }
}
