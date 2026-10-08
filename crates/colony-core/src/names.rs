//! Stable, friendly bot names derived from ids, so the same session is always
//! called the same thing.

const MAIN: &[&str] = &[
    "Atlas", "Pip", "Moss", "Rook", "Juno", "Kite", "Vale", "Ember", "Quill", "Ash", "Bramble", "Cinder",
    "Dot", "Fern", "Gale", "Hazel", "Iris", "Jasper", "Koa", "Lark", "Miso", "Nimbus", "Olive", "Pebble",
    "Quinn", "Rumi", "Sorrel", "Tansy", "Umber", "Vesper", "Wren", "Yarrow", "Zephyr", "Basil", "Clover",
    "Dune", "Flint", "Ginger", "Holly", "Indigo", "Juniper", "Kestrel", "Linden", "Maple", "Nettle", "Onyx",
    "Pike", "Reed", "Sage", "Thistle",
];

/// FNV-1a; stable across platforms and releases, unlike `DefaultHasher`.
pub fn stable_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn main_name(session_id: &str) -> String {
    MAIN[(stable_hash(session_id) % MAIN.len() as u64) as usize].to_string()
}

/// Subagents are named after their parent and type: "Moss · Explore 2".
pub fn sub_name(parent_name: &str, agent_type: Option<&str>, ordinal: usize) -> String {
    format!("{} · {} {}", parent_name, agent_type.unwrap_or("worker"), ordinal)
}
