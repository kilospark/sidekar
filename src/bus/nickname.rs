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

pub fn pick_nickname_for_project(project: Option<&str>) -> String {
    use rand::seq::SliceRandom;

    let mut used: HashSet<String> = HashSet::new();
    if let Ok(agents) = broker::list_agents(None) {
        for agent in agents {
            if let Some(nick) = agent.id.nick {
                used.insert(nick);
            }
        }
    }

    // Try to reuse stored nickname for this project (only if not already taken)

    if let Some(proj) = project {
        let nick_key = format!("_nick:{}", proj);
        let stored = broker::kv_get(&nick_key).ok().flatten().map(|e| e.value);

        if let Some(nick) = stored
            && !used.contains(&nick)
        {
            return nick; // reused successfully
        }
        // stored nickname is taken - fall through to pick new one

        // Pick new from available
        let mut available: Vec<&str> = NICKNAMES
            .iter()
            .filter(|n| !used.contains(**n))
            .copied()
            .collect();
        available.shuffle(&mut rand::rng());
        let picked = available.first().map(|s| s.to_string()).unwrap_or_else(|| {
            let r: u16 = rand::random();
            format!("agent-{:04x}", r)
        });

        // Store whatever nickname we got assigned
        let _ = broker::kv_set(&nick_key, &picked, None);
        picked
    } else {
        // Standalone - no project, just pick random
        let mut available: Vec<&str> = NICKNAMES
            .iter()
            .filter(|n| !used.contains(**n))
            .copied()
            .collect();
        available.shuffle(&mut rand::rng());
        available.first().map(|s| s.to_string()).unwrap_or_else(|| {
            let r: u16 = rand::random();
            format!("agent-{:04x}", r)
        })
    }
}
