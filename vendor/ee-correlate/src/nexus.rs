//! Event Nexus — an ontological **causal graph** over the event stream.
//!
//! SitDeck's flagship paid differentiator is the *Event Nexus Explorer*: an "ontological
//! causal timeline" that pulls **entities** out of events (people / places / orgs / assets),
//! links events that are about the same actors and ground, and lets an analyst walk the chain
//! **backward to a root cause** and **forward to consequences** — a storyline, not a pin-cloud
//! (`sitdeck-features.md` *Special modes → Event Nexus Explorer*; capability-map: *Special
//! modes → Event Nexus — causal graph: entity extraction → backward/forward cascades → actor
//! networks*).
//!
//! ## How it relates to the rest of `ee-correlate`
//! [`crate::crossdomain`] answers "is this *one situation* showing up across *different feeds*?"
//! by grouping events into undirected components. The Nexus goes one step further and gives the
//! component **direction and structure**:
//!
//! - **Entity extraction** ([`extract_entities`]) — a deterministic, dictionary-free heuristic
//!   NER that lifts proper-noun runs from a title and types them (person / org / asset / place),
//!   so events can be linked by *who and what* they name, not just loose keywords.
//! - **Causal edges** — a directed edge `A → B` is drawn only **forward in time** (strict, so the
//!   graph is always a DAG — no event can cause an earlier one) when the two events are *related*
//!   (share entities or enough keywords) and fall within a `window`. Each edge's strength blends
//!   the topical affinity, a **geographic-correlation** bonus when both events are co-located, and
//!   a **recency weight** that decays with the time gap (recency-weighted temporal cascading).
//! - **Cascades** ([`Nexus::cascade`]) — from any focus event, walk the reverse edges to its
//!   **root causes** and the forward edges to its **terminal consequences**.
//! - **Actor network** ([`Nexus::actors`]) — the entity co-occurrence graph: which actors appear
//!   together, and which are the most-connected hubs.
//! - **Topical threads** ([`Nexus::threads`]) — the weakly-connected components of the causal
//!   graph: each is one storyline, its events in chronological order.
//!
//! Everything is pure: a slice of events in, a derived [`Nexus`] out, no I/O — fully testable
//! offline.

use chrono::{DateTime, Duration, Utc};
use ee_core::Event;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Capitalized words that are *not* entities — articles, prepositions, conjunctions, and the
/// common headline verbs/nouns that title-case capitalizes. A capitalized token whose lowercase
/// form is here breaks a proper-noun run, so "Russia Launches Strikes On Kyiv" yields the actors
/// `Russia` and `Kyiv`, not one giant blob. Lowercase comparison.
const COMMON_WORDS: &[&str] = &[
    // articles / conjunctions / prepositions / pronouns
    "the", "a", "an", "and", "or", "but", "nor", "for", "yet", "so", "of", "in", "on", "at", "to",
    "by", "as", "is", "it", "be", "no", "up", "off", "out", "with", "from", "into", "near", "over",
    "amid", "after", "before", "between", "across", "against", "around", "during", "without",
    "within", "this", "that", "these", "those", "their", "there", "what", "when", "where", "which",
    "while", "his", "her", "its", "our", "your", "they", "them", "has", "had", "have", "are",
    "was", "were", "been", "will", "would", "could", "should", "may", "more", "most", "new",
    "two", "three", "amid", "via",
    // common leading adjectives (headlines capitalize the first word; these are never entities)
    "major", "minor", "huge", "massive", "deadly", "heavy", "fresh", "key", "big", "top",
    "strong", "weak", "high", "low", "early", "late", "former", "several", "many", "first",
    "last", "next", "global", "national", "local", "rising", "growing",
    // common headline verbs (title-case capitalizes them)
    "says", "said", "report", "reports", "reported", "reporting", "update", "updates", "breaking",
    "latest", "launches", "launched", "launch", "strikes", "strike", "hits", "hit", "kills",
    "kill", "killed", "warns", "warn", "warned", "warning", "confirms", "confirm", "confirmed",
    "announces", "announced", "orders", "ordered", "halts", "halted", "surges", "surge", "surged",
    "jumps", "jump", "plunges", "plunge", "plunged", "rises", "rise", "rose", "falls", "fall",
    "fell", "rocks", "rocked", "shakes", "spreads", "spread", "spreading", "erupts", "erupt",
    "erupted", "evacuates", "evacuated", "evacuate", "deploys", "deployed", "deploy", "seizes",
    "seized", "calls", "called", "vows", "vowed", "claims", "claimed", "denies", "denied",
    "approves", "approved", "signs", "signed", "meets", "met", "holds", "held", "opens", "opened",
    "closes", "closed", "begins", "began", "ends", "ended", "leaves", "left", "sends", "sent",
    "faces", "faced", "sees", "seen", "adds", "added", "cuts", "cut", "raises", "raised",
    "expands", "expanded", "boosts", "boosted", "triggers", "triggered", "sparks", "sparked",
    "fuels", "fueled", "disrupts", "disrupted", "threatens", "threatened", "targets", "targeted",
    // common nouns that show up capitalized but aren't a named actor on their own
    "today", "tonight", "year", "week", "day", "death", "toll", "people", "thousands", "millions",
    "officials", "official", "government", "talks", "deal", "crisis", "alert", "news", "live",
];

/// Personal-title words: a capitalized proper-noun run *immediately after* one of these is a
/// person ("President Zelensky", "Minister Lavrov", "General Kim").
const PERSON_TITLES: &[&str] = &[
    "president", "minister", "pm", "premier", "chancellor", "king", "queen", "prince", "general",
    "gen", "colonel", "col", "admiral", "captain", "dr", "mr", "ms", "mrs", "sen", "senator",
    "rep", "representative", "governor", "mayor", "secretary", "ceo", "chairman", "ayatollah",
    "pope", "sheikh", "emir", "sultan", "chief",
];

/// Lowercase hints that type a proper-noun run as an **organization**.
const ORG_HINTS: &[&str] = &[
    "inc", "corp", "ltd", "llc", "plc", "group", "holdings", "company", "co", "agency", "ministry",
    "bank", "army", "navy", "force", "forces", "command", "council", "union", "party", "guard",
    "brigade", "division", "battalion", "corps", "university", "department", "court", "police",
    "federation", "federal", "reserve", "fund", "bureau", "committee", "coalition", "alliance",
    "front", "movement", "cartel", "syndicate", "authority", "commission", "parliament", "senate",
    "congress", "assembly", "nato", "opec", "imf", "fed", "ecb", "fbi", "cia", "nasa", "un",
];

/// Lowercase hints that type a proper-noun run as a physical **asset / facility**.
const ASSET_HINTS: &[&str] = &[
    "port", "airport", "plant", "pipeline", "bridge", "dam", "station", "base", "refinery",
    "terminal", "reactor", "field", "mine", "factory", "depot", "grid", "cable", "rail",
    "railway", "highway", "canal", "strait", "channel", "tunnel", "harbor", "harbour", "facility",
    "complex", "site", "warehouse", "substation", "powerplant",
];

/// The kind of a named entity lifted from an event title.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Person,
    Org,
    Place,
    Asset,
}

impl EntityKind {
    pub fn label(self) -> &'static str {
        match self {
            EntityKind::Person => "person",
            EntityKind::Org => "org",
            EntityKind::Place => "place",
            EntityKind::Asset => "asset",
        }
    }
}

/// A named entity extracted from an event title.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entity {
    /// Display form, as it appeared in the title (e.g. `"Hodeidah Port"`).
    pub name: String,
    /// Lowercase match key used to link events (e.g. `"hodeidah port"`).
    pub key: String,
    pub kind: EntityKind,
}

/// Tunables for [`build`].
#[derive(Debug, Clone)]
pub struct NexusParams {
    /// Maximum time gap between two events for a causal edge to be possible.
    pub window: Duration,
    /// Recency half-life: an edge's strength halves for every `half_life` of time gap, so a
    /// consequence that follows hard on its cause links more strongly than a distant echo.
    pub half_life: Duration,
    /// Keyword-only links need at least this many shared significant keywords; a single shared
    /// **entity** always qualifies regardless. Clamped to a floor of 1.
    pub min_shared_keywords: usize,
    /// Co-location radius (km): two located events within it get a geographic-correlation bonus.
    pub geo_km: f64,
    /// Strength bonus (in `[0,1]`) for full co-location, scaled down with distance.
    pub geo_weight: f64,
    /// Saturation scale for topical affinity (`2·entities + keywords`). Larger = needs more
    /// overlap to saturate. Clamped to a floor of `0.5`.
    pub affinity_scale: f64,
    /// Minimum edge strength to keep. Clamped to `[0, 1]`.
    pub min_strength: f64,
}

impl Default for NexusParams {
    /// A 48 h causal window with a 12 h recency half-life catches the lag from a cause to its
    /// downstream effects; two shared keywords (or one shared entity) to link; co-location within
    /// 250 km adds up to `0.25`; edges below `0.15` strength are dropped as noise.
    fn default() -> Self {
        Self {
            window: Duration::hours(48),
            half_life: Duration::hours(12),
            min_shared_keywords: 2,
            geo_km: 250.0,
            geo_weight: 0.25,
            affinity_scale: 2.0,
            min_strength: 0.15,
        }
    }
}

/// A directed causal edge `from → to` (always forward in time).
#[derive(Debug, Clone, Serialize)]
pub struct CausalEdge {
    /// Index of the cause (earlier) event in [`Nexus::events`].
    pub from: usize,
    /// Index of the effect (later) event in [`Nexus::events`].
    pub to: usize,
    /// Edge strength in `(0, 1]` = topical affinity ⊕ geo bonus, ⊗ recency decay.
    pub strength: f64,
    /// Entity keys shared by both ends — *who/what* links them.
    pub shared_entities: Vec<String>,
    /// Significant keywords shared by both ends.
    pub shared_keywords: Vec<String>,
    /// Great-circle distance (km) between the two ends, when both are located.
    pub distance_km: Option<f64>,
    /// Time gap from cause to effect, in seconds.
    pub gap_secs: i64,
}

/// One node of the actor co-occurrence network.
#[derive(Debug, Clone, Serialize)]
pub struct ActorNode {
    pub entity: Entity,
    /// Number of events mentioning this entity.
    pub mentions: usize,
    /// Indices of those events, chronological.
    pub events: Vec<usize>,
}

/// A weighted, undirected link between two co-occurring actors.
#[derive(Debug, Clone, Serialize)]
pub struct ActorLink {
    pub a: String,
    pub b: String,
    /// Number of events in which both actors appear together.
    pub weight: usize,
}

/// The entity co-occurrence graph: who appears with whom.
#[derive(Debug, Clone, Serialize)]
pub struct ActorNetwork {
    /// Actors, most-mentioned first.
    pub nodes: Vec<ActorNode>,
    /// Co-occurrence links, heaviest first.
    pub links: Vec<ActorLink>,
}

/// A topical thread — one weakly-connected storyline of the causal graph.
#[derive(Debug, Clone, Serialize)]
pub struct Thread {
    /// Member event indices, chronological.
    pub events: Vec<usize>,
    /// The thread's leading entities (most-mentioned first, up to 5).
    pub entities: Vec<Entity>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// Peak member severity.
    pub peak: f64,
}

impl Thread {
    pub fn size(&self) -> usize {
        self.events.len()
    }
}

/// The result of walking the causal graph out from a focus event.
#[derive(Debug, Clone, Serialize)]
pub struct Cascade {
    /// The focus event index.
    pub focus: usize,
    /// All ancestors (causes), chronological — the backward cascade.
    pub upstream: Vec<usize>,
    /// All descendants (consequences), chronological — the forward cascade.
    pub downstream: Vec<usize>,
    /// Root cause(s): ancestors with no cause of their own (graph in-degree 0). If the focus has
    /// no ancestors it is its own root.
    pub roots: Vec<usize>,
    /// Terminal consequence(s): descendants with no further effect (graph out-degree 0). If the
    /// focus has no descendants it is its own leaf.
    pub leaves: Vec<usize>,
}

/// The full Event Nexus: the events, the causal graph over them, and its derived structures.
#[derive(Debug, Clone, Serialize)]
pub struct Nexus {
    /// Events in input order; all indices in this struct refer here.
    pub events: Vec<Event>,
    /// Per-event extracted entities (parallel to `events`).
    pub entities: Vec<Vec<Entity>>,
    /// The directed causal edges, strongest-first.
    pub edges: Vec<CausalEdge>,
}

impl Nexus {
    /// Number of events in the graph.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Find an event's index by id.
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.events.iter().position(|e| e.id == id)
    }

    /// Walk the causal graph out from the event with the given id: backward to root cause(s),
    /// forward to terminal consequence(s). Returns `None` if the id is unknown.
    pub fn cascade(&self, focus_id: &str) -> Option<Cascade> {
        let focus = self.index_of(focus_id)?;
        let upstream = self.reachable(focus, Direction::Backward);
        let downstream = self.reachable(focus, Direction::Forward);

        // Roots: ancestors that themselves have no incoming edge. Leaves: descendants with no
        // outgoing edge. Fall back to the focus itself when the relevant side is empty.
        let mut roots: Vec<usize> =
            upstream.iter().copied().filter(|&i| self.in_degree(i) == 0).collect();
        if roots.is_empty() {
            roots.push(focus);
        }
        let mut leaves: Vec<usize> =
            downstream.iter().copied().filter(|&i| self.out_degree(i) == 0).collect();
        if leaves.is_empty() {
            leaves.push(focus);
        }
        roots.sort_by(|&a, &b| self.events[a].time.cmp(&self.events[b].time).then(a.cmp(&b)));
        leaves.sort_by(|&a, &b| self.events[a].time.cmp(&self.events[b].time).then(a.cmp(&b)));

        Some(Cascade { focus, upstream, downstream, roots, leaves })
    }

    /// The actor co-occurrence network over all extracted entities.
    pub fn actors(&self) -> ActorNetwork {
        // Mentions and the events each entity appears in (deduped per event by key).
        let mut nodes: BTreeMap<String, (Entity, Vec<usize>)> = BTreeMap::new();
        let mut links: BTreeMap<(String, String), usize> = BTreeMap::new();

        for (i, ents) in self.entities.iter().enumerate() {
            // Unique entity keys in this event, with a representative display entity.
            let mut seen: BTreeMap<String, Entity> = BTreeMap::new();
            for e in ents {
                seen.entry(e.key.clone()).or_insert_with(|| e.clone());
            }
            for (key, ent) in &seen {
                let slot = nodes.entry(key.clone()).or_insert_with(|| (ent.clone(), Vec::new()));
                slot.1.push(i);
            }
            // Co-occurrence: every unordered pair of distinct entities in this event.
            let keys: Vec<&String> = seen.keys().collect();
            for a in 0..keys.len() {
                for b in (a + 1)..keys.len() {
                    let pair = (keys[a].clone(), keys[b].clone());
                    *links.entry(pair).or_insert(0) += 1;
                }
            }
        }

        let mut node_vec: Vec<ActorNode> = nodes
            .into_values()
            .map(|(entity, events)| ActorNode { mentions: events.len(), entity, events })
            .collect();
        node_vec.sort_by(|x, y| {
            y.mentions.cmp(&x.mentions).then(x.entity.key.cmp(&y.entity.key))
        });

        let mut link_vec: Vec<ActorLink> = links
            .into_iter()
            .map(|((a, b), weight)| ActorLink { a, b, weight })
            .collect();
        link_vec.sort_by(|x, y| {
            y.weight.cmp(&x.weight).then(x.a.cmp(&y.a)).then(x.b.cmp(&y.b))
        });

        ActorNetwork { nodes: node_vec, links: link_vec }
    }

    /// The topical threads — weakly-connected components of the causal graph with ≥2 events.
    /// Returns `(threads, isolated)`: threads ranked by peak severity then size, and the count of
    /// events that belong to no edge (lone pins).
    pub fn threads(&self) -> (Vec<Thread>, usize) {
        let n = self.events.len();
        // Union-find over undirected edges.
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(parent: &mut [usize], mut x: usize) -> usize {
            while parent[x] != x {
                parent[x] = parent[parent[x]];
                x = parent[x];
            }
            x
        }
        for e in &self.edges {
            let (ra, rb) = (find(&mut parent, e.from), find(&mut parent, e.to));
            if ra != rb {
                parent[ra] = rb;
            }
        }
        let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for i in 0..n {
            let r = find(&mut parent, i);
            groups.entry(r).or_default().push(i);
        }

        let mut isolated = 0usize;
        let mut threads: Vec<Thread> = Vec::new();
        for (_, mut members) in groups {
            if members.len() < 2 {
                isolated += members.len();
                continue;
            }
            members.sort_by(|&a, &b| {
                self.events[a].time.cmp(&self.events[b].time).then(a.cmp(&b))
            });
            let start = self.events[members[0]].time;
            let end = self.events[*members.last().unwrap()].time;
            let peak = members
                .iter()
                .map(|&i| self.events[i].severity.value())
                .fold(0.0_f64, f64::max);
            let entities = self.top_entities(&members, 5);
            threads.push(Thread { events: members, entities, start, end, peak });
        }
        threads.sort_by(|x, y| {
            y.peak
                .partial_cmp(&x.peak)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(y.size().cmp(&x.size()))
                .then(x.start.cmp(&y.start))
        });
        (threads, isolated)
    }

    // ---- internals ----

    fn in_degree(&self, i: usize) -> usize {
        self.edges.iter().filter(|e| e.to == i).count()
    }
    fn out_degree(&self, i: usize) -> usize {
        self.edges.iter().filter(|e| e.from == i).count()
    }

    /// All nodes reachable from `start` following edges in the given direction (excluding
    /// `start`), returned chronological.
    fn reachable(&self, start: usize, dir: Direction) -> Vec<usize> {
        let mut seen: BTreeSet<usize> = BTreeSet::new();
        let mut queue: VecDeque<usize> = VecDeque::new();
        queue.push_back(start);
        let mut visited_start = false;
        while let Some(cur) = queue.pop_front() {
            if visited_start && !seen.insert(cur) {
                continue;
            }
            visited_start = true;
            for e in &self.edges {
                let next = match dir {
                    Direction::Forward if e.from == cur => Some(e.to),
                    Direction::Backward if e.to == cur => Some(e.from),
                    _ => None,
                };
                if let Some(nx) = next {
                    if nx != start && !seen.contains(&nx) {
                        queue.push_back(nx);
                    }
                }
            }
        }
        let mut out: Vec<usize> = seen.into_iter().collect();
        out.sort_by(|&a, &b| self.events[a].time.cmp(&self.events[b].time).then(a.cmp(&b)));
        out
    }

    /// The most-mentioned entities across the given event indices, up to `n`.
    fn top_entities(&self, members: &[usize], n: usize) -> Vec<Entity> {
        let mut counts: BTreeMap<String, (Entity, usize)> = BTreeMap::new();
        for &i in members {
            let mut seen: BTreeSet<String> = BTreeSet::new();
            for e in &self.entities[i] {
                if seen.insert(e.key.clone()) {
                    let slot = counts.entry(e.key.clone()).or_insert_with(|| (e.clone(), 0));
                    slot.1 += 1;
                }
            }
        }
        let mut v: Vec<(Entity, usize)> = counts.into_values().collect();
        v.sort_by(|x, y| y.1.cmp(&x.1).then(x.0.key.cmp(&y.0.key)));
        v.into_iter().take(n).map(|(e, _)| e).collect()
    }
}

#[derive(Clone, Copy)]
enum Direction {
    Forward,
    Backward,
}

/// Build the Event Nexus from a slice of events.
pub fn build(events: &[Event], params: &NexusParams) -> Nexus {
    let min_kw = params.min_shared_keywords.max(1);
    let scale = params.affinity_scale.max(0.5);
    let min_strength = params.min_strength.clamp(0.0, 1.0);
    let half_life = params.half_life.num_seconds().max(1) as f64;

    let events: Vec<Event> = events.to_vec();
    let entities: Vec<Vec<Entity>> = events.iter().map(|e| extract_entities(&e.title)).collect();
    let entity_keys: Vec<BTreeSet<String>> = entities
        .iter()
        .map(|es| es.iter().map(|e| e.key.clone()).collect())
        .collect();
    let keywords: Vec<BTreeSet<String>> =
        events.iter().map(|e| keyword_tokens(&e.title)).collect();

    // Chronological order of indices, so we only consider earlier→later pairs.
    let mut order: Vec<usize> = (0..events.len()).collect();
    order.sort_by(|&a, &b| events[a].time.cmp(&events[b].time).then(a.cmp(&b)));

    let mut edges: Vec<CausalEdge> = Vec::new();
    for (oi, &i) in order.iter().enumerate() {
        for &j in &order[oi + 1..] {
            // Strict forward-in-time only; equal times are concurrent (no causal direction).
            let gap = events[j].time - events[i].time;
            if gap <= Duration::zero() || gap > params.window {
                continue;
            }
            let shared_ents: Vec<String> =
                entity_keys[i].intersection(&entity_keys[j]).cloned().collect();
            let shared_kw: Vec<String> =
                keywords[i].intersection(&keywords[j]).cloned().collect();

            // A causal link needs a topical basis: a shared entity, or enough shared keywords.
            if shared_ents.is_empty() && shared_kw.len() < min_kw {
                continue;
            }

            let topical = (2 * shared_ents.len() + shared_kw.len()) as f64;
            let affinity_topical = 1.0 - (-topical / scale).exp();

            let distance_km = match (events[i].geo, events[j].geo) {
                (Some(a), Some(b)) => Some(a.haversine_km(&b)),
                _ => None,
            };
            let geo_bonus = match distance_km {
                Some(d) if d <= params.geo_km => {
                    params.geo_weight * (1.0 - d / params.geo_km).max(0.0)
                }
                _ => 0.0,
            };

            let affinity = (affinity_topical + geo_bonus).min(1.0);
            let recency = 0.5_f64.powf(gap.num_seconds() as f64 / half_life);
            let strength = affinity * recency;
            if strength < min_strength {
                continue;
            }

            edges.push(CausalEdge {
                from: i,
                to: j,
                strength,
                shared_entities: shared_ents,
                shared_keywords: shared_kw,
                distance_km,
                gap_secs: gap.num_seconds(),
            });
        }
    }

    edges.sort_by(|a, b| {
        b.strength
            .partial_cmp(&a.strength)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.from.cmp(&b.from))
            .then(a.to.cmp(&b.to))
    });

    Nexus { events, entities, edges }
}

/// Significant lowercase keywords of a title (length ≥ 3, common words removed). Deduplicated.
fn keyword_tokens(title: &str) -> BTreeSet<String> {
    title
        .split(|c: char| !c.is_alphanumeric())
        .filter_map(|w| {
            if w.len() < 3 {
                return None;
            }
            let w = w.to_lowercase();
            if COMMON_WORDS.contains(&w.as_str()) {
                None
            } else {
                Some(w)
            }
        })
        .collect()
}

/// Extract named entities from a title with a deterministic, dictionary-free heuristic: maximal
/// runs of capitalized words (proper nouns) that are not [`COMMON_WORDS`], typed by small hint
/// lexicons (person-title prefix → [`EntityKind::Person`]; org/asset hints; else [`EntityKind::Place`]).
pub fn extract_entities(title: &str) -> Vec<Entity> {
    // Tokenize into (clean_word, is_capitalized), preserving order.
    let words: Vec<&str> = title.split_whitespace().collect();
    let mut out: Vec<Entity> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    let mut run: Vec<String> = Vec::new();
    let mut person_flag = false;
    let mut pending_title = false; // previous token was a personal title

    let flush = |run: &mut Vec<String>,
                 person_flag: &mut bool,
                 out: &mut Vec<Entity>,
                 seen: &mut BTreeSet<String>| {
        if run.is_empty() {
            *person_flag = false;
            return;
        }
        let name = run.join(" ");
        let key = name.to_lowercase();
        run.clear();
        let is_person = std::mem::take(person_flag);
        if seen.insert(key.clone()) {
            let kind = classify_entity(&key, is_person);
            out.push(Entity { name, key, kind });
        }
    };

    for raw in words {
        let clean = raw.trim_matches(|c: char| !c.is_alphanumeric());
        if clean.is_empty() {
            flush(&mut run, &mut person_flag, &mut out, &mut seen);
            pending_title = false;
            continue;
        }
        let lower = clean.to_lowercase();
        let first = clean.chars().next().unwrap();
        let is_cap = first.is_uppercase() && clean.chars().count() >= 2;
        let is_common = COMMON_WORDS.contains(&lower.as_str());

        // A personal title ("President", "General", "Dr") is not itself an entity — it ends any
        // current run and primes the *next* capitalized run as a person.
        if PERSON_TITLES.contains(&lower.as_str()) {
            flush(&mut run, &mut person_flag, &mut out, &mut seen);
            pending_title = true;
            continue;
        }

        if pending_title {
            person_flag = true;
        }

        if is_cap && !is_common {
            run.push(clean.to_string());
            pending_title = false;
        } else {
            flush(&mut run, &mut person_flag, &mut out, &mut seen);
            // A lowercase (or common) personal title primes the next capitalized run as a person.
            pending_title = PERSON_TITLES.contains(&lower.as_str());
        }
    }
    flush(&mut run, &mut person_flag, &mut out, &mut seen);
    out
}

/// Type a proper-noun run from its lowercase key.
fn classify_entity(key: &str, is_person: bool) -> EntityKind {
    if is_person {
        return EntityKind::Person;
    }
    let toks: Vec<&str> = key.split_whitespace().collect();
    if toks.iter().any(|t| ASSET_HINTS.contains(t)) {
        return EntityKind::Asset;
    }
    if toks.iter().any(|t| ORG_HINTS.contains(t)) {
        return EntityKind::Org;
    }
    EntityKind::Place
}

#[cfg(test)]
mod tests {
    use super::*;
    use ee_core::{EventKind, Geo, Severity};

    fn ev(id: &str, kind: EventKind, title: &str, mins: i64, geo: Option<(f64, f64)>, sev: f64) -> Event {
        let base = DateTime::parse_from_rfc3339("2026-06-08T00:00:00Z").unwrap().with_timezone(&Utc);
        Event {
            id: id.into(),
            source_id: "test".into(),
            kind,
            title: title.into(),
            time: base + Duration::minutes(mins),
            geo: geo.and_then(|(a, b)| Geo::new(a, b)),
            severity: Severity::new(sev),
            url: None,
            raw: serde_json::Value::Null,
        }
    }

    #[test]
    fn extracts_and_types_entities() {
        let ents = extract_entities("Russia launches strikes on Hodeidah Port near Kyiv");
        let keys: Vec<&str> = ents.iter().map(|e| e.key.as_str()).collect();
        assert!(keys.contains(&"russia"), "got {keys:?}");
        assert!(keys.contains(&"hodeidah port"), "got {keys:?}");
        assert!(keys.contains(&"kyiv"), "got {keys:?}");
        // verbs/prepositions are not entities
        assert!(!keys.iter().any(|k| k.contains("launches") || k.contains("strikes")));
        // typing
        let port = ents.iter().find(|e| e.key == "hodeidah port").unwrap();
        assert_eq!(port.kind, EntityKind::Asset);
        let russia = ents.iter().find(|e| e.key == "russia").unwrap();
        assert_eq!(russia.kind, EntityKind::Place);
    }

    #[test]
    fn detects_person_after_title() {
        let ents = extract_entities("President Zelensky meets General Kim");
        let z = ents.iter().find(|e| e.key == "zelensky").expect("zelensky");
        assert_eq!(z.kind, EntityKind::Person);
        let k = ents.iter().find(|e| e.key == "kim").expect("kim");
        assert_eq!(k.kind, EntityKind::Person);
    }

    #[test]
    fn types_org_by_hint() {
        let ents = extract_entities("Wagner Group expands operations");
        let w = ents.iter().find(|e| e.key == "wagner group").expect("wagner group");
        assert_eq!(w.kind, EntityKind::Org);
    }

    #[test]
    fn edges_only_go_forward_in_time() {
        // Two events sharing an entity; the edge must point earlier→later.
        let evs = vec![
            ev("a", EventKind::Conflict, "Strikes on Hodeidah Port reported", 0, None, 0.8),
            ev("b", EventKind::Market, "Oil jumps as Hodeidah Port disruption hits shipping", 60, None, 0.6),
        ];
        let nx = build(&evs, &NexusParams::default());
        assert_eq!(nx.edges.len(), 1);
        assert_eq!(nx.edges[0].from, 0);
        assert_eq!(nx.edges[0].to, 1);
        assert!(nx.edges[0].shared_entities.contains(&"hodeidah port".to_string()));
    }

    #[test]
    fn no_edge_without_topical_basis() {
        let evs = vec![
            ev("a", EventKind::Earthquake, "Earthquake near Tokyo", 0, Some((35.6, 139.7)), 0.7),
            ev("b", EventKind::Market, "Coffee prices climb in Brazil", 30, Some((35.6, 139.7)), 0.4),
        ];
        // Co-located in time+space but share no entity/keywords -> no causal edge.
        let nx = build(&evs, &NexusParams::default());
        assert!(nx.edges.is_empty(), "unrelated co-located events must not link: {:?}", nx.edges);
    }

    #[test]
    fn window_gates_edges() {
        let p = NexusParams { window: Duration::minutes(30), ..NexusParams::default() };
        let evs = vec![
            ev("a", EventKind::Conflict, "Strikes on Hodeidah Port", 0, None, 0.8),
            ev("b", EventKind::News, "Carriers reroute around Hodeidah Port", 120, None, 0.5),
        ];
        let nx = build(&evs, &p);
        assert!(nx.edges.is_empty(), "edge outside window should be dropped");
    }

    #[test]
    fn cascade_finds_root_and_consequences() {
        // Quake -> tsunami warning -> market drop, chained on shared entities.
        let evs = vec![
            ev("q", EventKind::Earthquake, "Major earthquake strikes Tokyo region", 0, Some((35.6, 139.7)), 0.9),
            ev("w", EventKind::News, "Tokyo earthquake triggers tsunami warning", 30, None, 0.7),
            ev("m", EventKind::Market, "Nikkei tumbles after Tokyo earthquake", 90, None, 0.6),
        ];
        let nx = build(&evs, &NexusParams::default());
        let c = nx.cascade("m").expect("focus m");
        assert_eq!(c.focus, 2);
        // m's upstream should include the quake (root) and the warning.
        assert!(c.upstream.contains(&0));
        assert_eq!(c.roots, vec![0]);
        // From the quake, the consequence chain reaches the market.
        let c2 = nx.cascade("q").expect("focus q");
        assert!(c2.downstream.contains(&2));
        assert_eq!(c2.leaves, vec![2]);
    }

    #[test]
    fn actor_network_counts_cooccurrence() {
        let evs = vec![
            ev("a", EventKind::Conflict, "Russia strikes Kyiv", 0, None, 0.8),
            ev("b", EventKind::Conflict, "Russia shells Kyiv suburbs", 60, None, 0.7),
            ev("c", EventKind::News, "Poland reinforces border", 120, None, 0.4),
        ];
        let nx = build(&evs, &NexusParams::default());
        let net = nx.actors();
        let russia = net.nodes.iter().find(|n| n.entity.key == "russia").unwrap();
        assert_eq!(russia.mentions, 2);
        let link = net.links.iter().find(|l| {
            (l.a == "kyiv" && l.b == "russia") || (l.a == "russia" && l.b == "kyiv")
        });
        assert_eq!(link.unwrap().weight, 2);
    }

    #[test]
    fn threads_separate_unrelated_storylines() {
        let evs = vec![
            // storyline 1: Hodeidah
            ev("a", EventKind::Conflict, "Strikes on Hodeidah Port", 0, None, 0.8),
            ev("b", EventKind::Market, "Oil jumps on Hodeidah Port disruption", 60, None, 0.6),
            // storyline 2: Tokyo
            ev("c", EventKind::Earthquake, "Earthquake rocks Tokyo region", 10, Some((35.6, 139.7)), 0.9),
            ev("d", EventKind::News, "Tokyo earthquake triggers evacuations", 40, None, 0.7),
            // lone pin
            ev("e", EventKind::Wildfire, "Wildfire near Athens", 20, Some((38.0, 23.7)), 0.5),
        ];
        let nx = build(&evs, &NexusParams::default());
        let (threads, isolated) = nx.threads();
        assert_eq!(threads.len(), 2, "two storylines");
        assert_eq!(isolated, 1, "the wildfire is a lone pin");
        // top thread by peak severity is the Tokyo quake (0.9)
        assert!(threads[0].peak >= 0.9 - 1e-9);
    }

    #[test]
    fn recency_makes_closer_links_stronger() {
        let near = vec![
            ev("a", EventKind::Conflict, "Strikes on Hodeidah Port", 0, None, 0.8),
            ev("b", EventKind::News, "Hodeidah Port shipping disrupted", 30, None, 0.6),
        ];
        let far = vec![
            ev("a", EventKind::Conflict, "Strikes on Hodeidah Port", 0, None, 0.8),
            ev("b", EventKind::News, "Hodeidah Port shipping disrupted", 600, None, 0.6),
        ];
        let sn = build(&near, &NexusParams::default()).edges[0].strength;
        let sf = build(&far, &NexusParams::default()).edges[0].strength;
        assert!(sn > sf, "closer-in-time edge should be stronger ({sn} vs {sf})");
    }
}
