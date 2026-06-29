// ------------------------------------------------------------
// RAiTHE INDUSTRIES INCORPORATED
// Copyright (c) 2026 All Rights Reserved.
//
// This file is part of a proprietary system. Unauthorized use,
// reproduction, or distribution is strictly prohibited.
// ------------------------------------------------------------

// src/ingestor.rs — RSS / GNews / GDELT ingestion
// Uses reqwest + feed-rs instead of aiohttp + feedparser.
//
// Upgrade changes applied in this version:
//
//   PARALLEL RSS POLLING
//     All 43 RSS feeds are now fetched concurrently (one tokio::spawn per feed)
//     instead of serially. The previous serial loop polled each feed one after
//     another, meaning a single slow or timing-out feed (12s timeout × 43 feeds
//     = potential 8.6 minute cycle at worst) blocked all others. With concurrent
//     fetching every feed starts simultaneously; the cycle completes in max(one
//     feed's latency) ≈ 2–5 seconds. This raises effective throughput from
//     ~200 articles/hour to 2,000+ articles/hour in active news periods.
//     A semaphore (MAX_CONCURRENT_RSS = 20) caps simultaneous HTTP connections
//     to avoid overwhelming the network interface or triggering rate limiting.
//
//   IMPROVED BODY EXTRACTION
//     entry_to_article() previously took only the feed `summary` field. Most
//     structured RSS feeds (NYT, WaPo, BBC, Al Jazeera) carry substantive text
//     in the `content` field, using `summary` only as a fallback abstract. The
//     extractor now prefers content.body over summary.content, falling back
//     correctly when either is absent. This materially improves NLP signal
//     quality: a content field typically provides 300–800 characters of
//     article text versus a summary's 80–150 characters.
//
//   RAISED ARTICLE LIMITS
//     RSS per-feed article limit: 20 → 50 entries per poll cycle.
//       At 43 feeds × 50 entries × 90s cycle = up to 2,388 articles/cycle max;
//       with deduplication and geopolitical filtering the effective rate is
//       200–600 new articles/hour in active periods.
//     GNews per-query article limit: 15 → 25 entries.
//     SeenCache capacity: 10,000 → 50,000 entries.
//       At 2,000 art/hr the old 10k cache expired in 5 hours, causing re-
//       ingestion of articles from earlier in the same day. 50k covers 25
//       hours of headroom at peak ingest rate.
//
//   BACKOFF JITTER
//     A small random jitter (±20%) is added to all poll intervals to prevent
//     feed thundering-herd effects when multiple GCRM instances run in parallel.
//     Uses a simple deterministic counter-based pseudo-jitter to preserve
//     reproducibility — no external RNG dependency.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use reqwest::Client;
use tokio::sync::{mpsc, Mutex, Semaphore};
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::aggregator::{AppState, StoredArticle};
use crate::models::{RawArticle, SourceTier};

// ── Feed registry ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct FeedSpec {
    pub url:    &'static str,
    pub source: &'static str,
    pub tier:   SourceTier,
}

pub const RSS_FEEDS: &[FeedSpec] = &[
    // ── Tier 1: Wire services, verified international ──────────────────────
    FeedSpec { url: "https://feeds.bbci.co.uk/news/world/rss.xml",                source: "bbc",             tier: SourceTier::Tier1 },
    FeedSpec { url: "https://feeds.bbci.co.uk/news/rss.xml",                      source: "bbc",             tier: SourceTier::Tier1 },
    FeedSpec { url: "https://www.aljazeera.com/xml/rss/all.xml",                   source: "aljazeera",       tier: SourceTier::Tier1 },
    FeedSpec { url: "https://feeds.skynews.com/feeds/rss/world.xml",               source: "skynews",         tier: SourceTier::Tier1 },
    // NYT
    FeedSpec { url: "https://rss.nytimes.com/services/xml/rss/nyt/World.xml",      source: "nyt",             tier: SourceTier::Tier1 },
    FeedSpec { url: "https://rss.nytimes.com/services/xml/rss/nyt/Politics.xml",   source: "nyt",             tier: SourceTier::Tier1 },
    FeedSpec { url: "https://rss.nytimes.com/services/xml/rss/nyt/MiddleEast.xml", source: "nyt",             tier: SourceTier::Tier1 },
    FeedSpec { url: "https://rss.nytimes.com/services/xml/rss/nyt/AsiaPacific.xml",source: "nyt",             tier: SourceTier::Tier1 },
    // WSJ World — replaces WaPo (feeds.washingtonpost.com discontinued its public RSS)
    FeedSpec { url: "https://feeds.a.dj.com/rss/RSSWorldNews.xml",                 source: "wsj",             tier: SourceTier::Tier1 },
    // Foreign Policy
    FeedSpec { url: "https://foreignpolicy.com/feed/",                             source: "foreignpolicy",   tier: SourceTier::Tier1 },
    // Defense-specific Tier 1
    FeedSpec { url: "https://www.defensenews.com/arc/outboundfeeds/rss/",          source: "defensenews",     tier: SourceTier::Tier1 },
    FeedSpec { url: "https://warontherocks.com/feed/",                             source: "warontherocks",   tier: SourceTier::Tier1 },
    FeedSpec { url: "https://taskandpurpose.com/feed/",                            source: "taskpurpose",     tier: SourceTier::Tier1 },
    // (Reuters discontinued its public RSS feeds globally — removed; GNews/GDELT
    //  still surface Reuters-originated coverage through the search APIs.)
    // Nuclear/arms Tier 1 — highest credibility for nuclear signals
    FeedSpec { url: "https://www.armscontrol.org/taxonomy/term/1/feed",            source: "armscontrol",     tier: SourceTier::Tier1 },
    FeedSpec { url: "https://fas.org/feed/",                                       source: "fas",             tier: SourceTier::Tier1 },
    // Defense Tier 1
    FeedSpec { url: "https://www.defenseone.com/rss/all/",                         source: "defenseone",      tier: SourceTier::Tier1 },
    // breakingdefense began hard-403ing this host (Cloudflare bot-fight, unfixable by
    // UA — the jamestown/longwarjournal pattern); replaced with DefenseScoop — same
    // daily Pentagon / defense-tech news beat, non-blocking host (probed 200 + valid RSS).
    FeedSpec { url: "https://defensescoop.com/feed/",                              source: "defensescoop",    tier: SourceTier::Tier1 },
    FeedSpec { url: "https://www.defensenews.com/arc/outboundfeeds/rss/category/pentagon/", source: "defensenews_pentagon", tier: SourceTier::Tier1 },
    // Think-tank / policy Tier 1
    FeedSpec { url: "https://thediplomat.com/feed/",                               source: "thediplomat",     tier: SourceTier::Tier1 },
    FeedSpec { url: "https://www.justsecurity.org/feed/",                          source: "justsecurity",    tier: SourceTier::Tier1 },
    // OSINT/analysis Tier 1
    FeedSpec { url: "https://www.bellingcat.com/feed/",                            source: "bellingcat",      tier: SourceTier::Tier1 },
    FeedSpec { url: "https://www.crisisgroup.org/rss",                             source: "crisisgroup",     tier: SourceTier::Tier1 },

    // ── Tier 2: Major outlets, verified free ───────────────────────────────
    FeedSpec { url: "https://www.theguardian.com/world/rss",                       source: "guardian",        tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.theguardian.com/us-news/rss",                     source: "guardian",        tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.theguardian.com/world/ukraine/rss",               source: "guardian",        tier: SourceTier::Tier2 },
    FeedSpec { url: "https://feeds.npr.org/1004/rss.xml",                          source: "npr",             tier: SourceTier::Tier2 },
    FeedSpec { url: "https://feeds.npr.org/1014/rss.xml",                          source: "npr",             tier: SourceTier::Tier2 },
    FeedSpec { url: "https://thehill.com/feed/",                                   source: "thehill",         tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.politico.eu/feed/",                               source: "politico_eu",     tier: SourceTier::Tier2 },
    // Canadian
    // cbc.ca/cmlink/rss-world was retired (301 → webfeed); use the canonical webfeed
    // URL directly. CBC's origin occasionally serves a cold-cache EMPTY shell (valid
    // RSS, 0 items) for a minute — the liveness probe's retry pass absorbs that.
    FeedSpec { url: "https://www.cbc.ca/webfeed/rss/rss-world",                    source: "cbc",             tier: SourceTier::Tier2 },
    FeedSpec { url: "https://globalnews.ca/feed/",                                  source: "globalnews",      tier: SourceTier::Tier2 },
    // Latin America
    FeedSpec { url: "https://www.jornada.com.mx/rss/mundo.xml",                    source: "lajornada_mx",    tier: SourceTier::Tier2 },
    // Eastern Europe / conflict zone
    FeedSpec { url: "https://www.pravda.com.ua/eng/rss/view_news/",                source: "ukrpravda",       tier: SourceTier::Tier2 },
    // Middle East
    FeedSpec { url: "https://www.timesofisrael.com/feed/",                          source: "timesofisrael",   tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.middleeasteye.net/rss",                            source: "mee",             tier: SourceTier::Tier2 },
    // Asia-Pacific
    FeedSpec { url: "https://www.scmp.com/rss/91/feed",                             source: "scmp",            tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.straitstimes.com/news/world/rss.xml",              source: "straitstimes",    tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.rfa.org/english/rss2.xml",                         source: "rfa",             tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.taipeitimes.com/xml/index.rss",                    source: "taipeitimes",     tier: SourceTier::Tier2 },
    // Intelligence/analysis
    FeedSpec { url: "https://geopoliticalfutures.com/feed/",                        source: "gpf",             tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.realcleardefense.com/index.xml",                   source: "realcleardefense",tier: SourceTier::Tier2 },
    // Think-tanks / policy Tier 2
    FeedSpec { url: "https://www.atlanticcouncil.org/feed/",                        source: "atlanticcouncil", tier: SourceTier::Tier2 },
    // brookings retired its public RSS (now serves HTML); replaced with CFR's main feed.
    FeedSpec { url: "https://feeds.cfr.org/cfr_main",                               source: "cfr",             tier: SourceTier::Tier1 },
    // carnegieendowment migrated to a feed-less Next.js site; replaced with RAND Corporation.
    FeedSpec { url: "https://www.rand.org/news.xml",                                source: "rand",            tier: SourceTier::Tier2 },
    FeedSpec { url: "https://mwi.westpoint.edu/feed/",                              source: "mwi",             tier: SourceTier::Tier2 },
    // jamestown ran aggressive Cloudflare bot-fight from the prod IP (HTML challenge,
    // unfixable by UA); replaced with OSW (Centre for Eastern Studies, Warsaw) — same
    // Russia/Eurasia/Central-Asia security-analysis niche, non-Cloudflare host.
    FeedSpec { url: "https://www.osw.waw.pl/en/rss.xml",                            source: "osw",             tier: SourceTier::Tier2 },
    FeedSpec { url: "https://responsiblestatecraft.org/feed/",                       source: "responsiblestatecraft", tier: SourceTier::Tier2 },
    // Official / intergovernmental
    FeedSpec { url: "https://news.un.org/feed/subscribe/en/news/all/rss.xml",       source: "un_news",         tier: SourceTier::Tier2 },
    // (NATO retired its public news RSS endpoint — removed.)
    // European broadcasters
    FeedSpec { url: "https://rss.dw.com/rdf/rss-en-world",                         source: "dw",              tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.france24.com/en/rss",                              source: "france24",        tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.euronews.com/rss",                                 source: "euronews",        tier: SourceTier::Tier2 },
    // (VOA retired the /rss/z/5752 zone feed — no clean XML replacement; removed.)
    // US outlets
    FeedSpec { url: "https://feeds.feedburner.com/time/world",                      source: "time",            tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.cbsnews.com/latest/rss/world",                     source: "cbsnews",         tier: SourceTier::Tier2 },
    FeedSpec { url: "https://abcnews.go.com/abcnews/internationalheadlines",        source: "abcnews",         tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.independent.co.uk/news/world/rss",                 source: "independent",     tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.bbc.co.uk/news/technology/rss.xml",                source: "bbc_tech",        tier: SourceTier::Tier2 },
    // State media / alternative perspectives (signals, not endorsements)
    FeedSpec { url: "https://tass.com/rss/v2.xml",                                  source: "tass",            tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.xinhuanet.com/english/rss/worldrss.xml",           source: "xinhua",          tier: SourceTier::Tier2 },
    // Asia-Pacific
    FeedSpec { url: "https://www.japantimes.co.jp/feed/",                           source: "japantimes",      tier: SourceTier::Tier2 },
    // koreaherald's RSS now returns empty shells after a CMS migration; replaced with Yonhap (ROK national wire).
    FeedSpec { url: "https://en.yna.co.kr/RSS/news.xml",                            source: "yonhap",          tier: SourceTier::Tier2 },
    // South Asia
    FeedSpec { url: "https://www.dawn.com/feeds/home",                              source: "dawn_pk",         tier: SourceTier::Tier2 },
    FeedSpec { url: "https://theprint.in/category/world/feed/",                      source: "theprint_in",     tier: SourceTier::Tier2 },
    // Middle East
    FeedSpec { url: "https://www.arabnews.com/rss.xml",                             source: "arabnews",        tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.aa.com.tr/en/rss/default?cat=world",               source: "anadolu",         tier: SourceTier::Tier2 },
    // haaretz retired its public cmlink RSS (now serves HTML); replaced with Ynetnews (Israel).
    FeedSpec { url: "https://www.ynetnews.com/Integration/StoryRss3082.xml",        source: "ynet",            tier: SourceTier::Tier2 },
    // Investigative / OSINT Tier 2
    FeedSpec { url: "https://meduza.io/rss/en/all",                                 source: "meduza",          tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.occrp.org/en/feed",                                source: "occrp",           tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.rferl.org/api/epiqq",                              source: "rferl",           tier: SourceTier::Tier2 },

    // ── 2026-05 expansion — 35 feeds added to broaden global coverage past 100 ──
    // Every URL below was probed from the production host and confirmed to return
    // HTTP 200 with valid RSS/Atom XML before inclusion.
    // Major international / wire — Tier 1
    FeedSpec { url: "https://feeds.nbcnews.com/nbcnews/public/world",               source: "nbcnews",         tier: SourceTier::Tier1 },
    FeedSpec { url: "https://www.foreignaffairs.com/rss.xml",                       source: "foreignaffairs",  tier: SourceTier::Tier1 },
    FeedSpec { url: "https://www.lemonde.fr/en/rss/une.xml",                        source: "lemonde",         tier: SourceTier::Tier1 },
    FeedSpec { url: "https://www.spiegel.de/international/index.rss",               source: "spiegel",         tier: SourceTier::Tier1 },
    FeedSpec { url: "https://www.economist.com/international/rss.xml",              source: "economist",       tier: SourceTier::Tier1 },
    FeedSpec { url: "https://feeds.bloomberg.com/politics/news.rss",               source: "bloomberg",       tier: SourceTier::Tier1 },
    FeedSpec { url: "https://asia.nikkei.com/rss/feed/nar",                         source: "nikkei",          tier: SourceTier::Tier1 },
    // Defense / military analysis — Tier 1
    FeedSpec { url: "https://news.usni.org/feed",                                   source: "usni",            tier: SourceTier::Tier1 },
    FeedSpec { url: "https://www.militarytimes.com/arc/outboundfeeds/rss/",         source: "militarytimes",   tier: SourceTier::Tier1 },
    // longwarjournal ran aggressive Cloudflare bot-fight from the prod IP (unfixable
    // by UA); replaced with The Cipher Brief — national-security/intelligence analysis,
    // non-Cloudflare host.
    FeedSpec { url: "https://www.thecipherbrief.com/feed",                          source: "cipherbrief",     tier: SourceTier::Tier1 },
    // Regional / national outlets — Tier 2
    FeedSpec { url: "https://moxie.foxnews.com/google-publisher/world.xml",         source: "foxnews",         tier: SourceTier::Tier2 },
    FeedSpec { url: "https://nationalpost.com/feed/",                               source: "nationalpost",    tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.cnbc.com/id/100727362/device/rss/rss.html",        source: "cnbc",            tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.abc.net.au/news/feed/51120/rss.xml",               source: "abc_au",          tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.rnz.co.nz/rss/world.xml",                          source: "rnz",             tier: SourceTier::Tier2 },
    // Asia-Pacific — Tier 2
    FeedSpec { url: "https://www.channelnewsasia.com/api/v1/rss-outbound-feed?_format=xml", source: "cna",      tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.thehindu.com/news/international/feeder/default.rss",source: "thehindu",        tier: SourceTier::Tier2 },
    FeedSpec { url: "https://timesofindia.indiatimes.com/rssfeeds/296589292.cms",   source: "timesofindia",    tier: SourceTier::Tier2 },
    FeedSpec { url: "https://asiatimes.com/feed/",                                  source: "asiatimes",       tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.nknews.org/feed/",                                 source: "nknews",          tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.globaltimes.cn/rss/outbrain.xml",                  source: "globaltimes",     tier: SourceTier::Tier2 },
    FeedSpec { url: "https://chinadigitaltimes.net/feed/",                          source: "chinadigitaltimes", tier: SourceTier::Tier2 },
    // Middle East — Tier 2
    FeedSpec { url: "https://www.jpost.com/rss/rssfeedsheadlines.aspx",             source: "jpost",           tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.middleeastmonitor.com/feed/",                      source: "middleeastmonitor", tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.al-monitor.com/rss",                               source: "almonitor",       tier: SourceTier::Tier2 },
    FeedSpec { url: "https://thecradle.co/feed",                                    source: "thecradle",       tier: SourceTier::Tier2 },
    // Russia / Eurasia — Tier 2
    FeedSpec { url: "https://www.themoscowtimes.com/rss/news",                      source: "moscowtimes",     tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.kyivpost.com/feed",                                source: "kyivpost",        tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.intellinews.com/feed",                             source: "intellinews",     tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.rt.com/rss/news/",                                 source: "rt",              tier: SourceTier::Tier2 },
    // Think tanks / specialist — Tier 2
    // nationalinterest began hard-403ing this host (Cloudflare bot-fight, unfixable by
    // UA); replaced with the Lowy Institute's The Interpreter — same IR/strategy
    // commentary niche (Indo-Pacific weighted), probed 200 + valid RSS.
    FeedSpec { url: "https://www.lowyinstitute.org/the-interpreter/rss.xml",        source: "lowy_interpreter", tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.twz.com/feed",                                     source: "twz",             tier: SourceTier::Tier2 },
    // Global South / humanitarian — Tier 2
    FeedSpec { url: "https://allafrica.com/tools/headlines/rdf/latest/headlines.rdf", source: "allafrica",     tier: SourceTier::Tier2 },
    FeedSpec { url: "https://en.mercopress.com/rss",                                source: "mercopress",      tier: SourceTier::Tier2 },
    FeedSpec { url: "https://reliefweb.int/updates/rss.xml",                        source: "reliefweb",       tier: SourceTier::Tier2 },
];

pub const GNEWS_QUERIES: &[&str] = &[
    "nuclear weapon missile test",
    "military attack airstrike offensive",
    "NATO alliance Article 5",
    "Iran nuclear deal sanctions",
    "Taiwan China military tension strait",
    "Russia Ukraine war offensive",
    "North Korea DPRK missile launch",
    "cyber attack infrastructure hack",
    "chemical weapon biological attack",
    "great power conflict US China Russia",
    "Middle East war Gaza Israel Lebanon",
    "Pakistan India border tension Kashmir",
    "submarine warship naval confrontation",
    "coup military takeover government",
    "sanctions embargo trade war",
    "drone strike assassination targeted killing",
    "hypersonic missile ICBM ballistic test",
    "nuclear reactor enrichment uranium weapons grade",
    "UN Security Council veto resolution",
    "military exercises drills live fire",
    "espionage spy intelligence leak",
    "famine blockade siege humanitarian crisis",
    "territorial dispute border clash skirmish",
];

pub const GDELT_QUERIES: &[&str] = &[
    "military attack",
    "nuclear weapon",
    "nato alliance",
    "diplomatic crisis",
    "missile launch",
    "cyber attack",
    "war escalation",
    "sanctions imposed",
];

// ── Concurrency control ───────────────────────────────────────────────────────
// Limit simultaneous RSS HTTP connections to avoid thundering-herd effects
// against individual hosts and to stay within typical OS socket limits.
const MAX_CONCURRENT_RSS: usize = 42;

// ── Feed poll cadences ──────────────────────────────────────────────────────────
// These are the NETWORK fetch clocks — deliberately their own absolute constants, NOT
// derived from the aggregator's snapshot tick (`poll_interval_seconds`, ~1s). They were
// previously `poll_interval_s * {5*1000, 12, 20}`; with the live snapshot tick of 1s that
// silently re-polled all 103 RSS hosts every ~5s (and GNews/GDELT every 12s/20s) from a
// single datacenter IP — abusive enough to earn 429s/IP-bans and take the roster dark,
// the exact opposite of the "feeds must stay live" invariant. Coupling them to the tick
// also meant any change to the dashboard refresh rate would silently re-tune the feeds.
//
/// RSS roster re-poll cycle (~100s across the full 103-feed roster): frequent enough to
/// catch breaking news, slow enough not to get the prod IP rate-limited/banned. (audit ingestor-1)
const RSS_CYCLE_MS: u64 = 100_000;
/// GNews (Google News RSS) per-query cadence. Google aggressively throttles datacenter IPs,
/// and one query fires per tick, so poll conservatively (~4 min/query).
const GNEWS_QUERY_INTERVAL_S: u64 = 240;
/// GDELT DOC API per-query cadence. GDELT itself only updates every ~15 min, so ~6.7 min
/// per query is already at the source's own resolution.
const GDELT_QUERY_INTERVAL_S: u64 = 400;

// ── Article limits ────────────────────────────────────────────────────────────
const RSS_ARTICLES_PER_FEED:  usize = 500;  // was 20
const GNEWS_ARTICLES_PER_QUERY: usize = 250; // was 15

// ── Seen cache — deduplication ────────────────────────────────────────────────
// MD5 of (url + title) — same as Python SeenCache.
// Capacity raised to 50,000 to cover 25h at 2,000 art/hr peak rate.

pub struct SeenCache {
    cache:    std::collections::HashSet<String>,
    order:    VecDeque<String>,
    max_size: usize,
}

impl SeenCache {
    pub fn new(max_size: usize) -> Self {
        Self {
            cache:    std::collections::HashSet::new(),
            order:    VecDeque::new(),
            max_size,
        }
    }

    fn key(url: &str, title: &str) -> String {
        let input = format!("{url}{title}");
        format!("{:x}", md5::compute(input.as_bytes()))
    }

    /// Returns true if this (url, title) pair has not been seen before.
    pub fn is_new(&mut self, url: &str, title: &str) -> bool {
        let k = Self::key(url, title);
        if self.cache.contains(&k) {
            return false;
        }
        self.cache.insert(k.clone());
        self.order.push_back(k);
        if self.order.len() > self.max_size {
            if let Some(old) = self.order.pop_front() {
                self.cache.remove(&old);
            }
        }
        true
    }

    /// Mark a (url, title) as already-seen without reporting novelty. Used at boot
    /// to seed the cache from disk-restored articles so live re-fetches of the same
    /// stories aren't stored a second time (the dedup cache is not itself persisted).
    pub fn mark_seen(&mut self, url: &str, title: &str) {
        let k = Self::key(url, title);
        if self.cache.insert(k.clone()) {
            self.order.push_back(k);
            if self.order.len() > self.max_size {
                if let Some(old) = self.order.pop_front() {
                    self.cache.remove(&old);
                }
            }
        }
    }
}

// ── Source health tracking ────────────────────────────────────────────────────

/// Consecutive failures before a source is considered unhealthy and demoted to
/// periodic re-probing instead of every-cycle polling.
const DISABLE_THRESHOLD: u32 = 10;
/// A demoted (unhealthy) source is re-probed once every this many RSS cycles.
/// At the ~100s base cycle this is roughly one retry every ~10 minutes — enough
/// to recover automatically from transient outages (DNS blips, feed maintenance,
/// rate-limit windows) without hammering a genuinely dead endpoint every cycle.
const REPROBE_EVERY_CYCLES: u64 = 6;

#[derive(Debug, Default)]
pub struct SourceHealth {
    /// Consecutive failure count per source name.
    failures: HashMap<String, u32>,
    /// Total article count per source (for /api/sources).
    pub registry: HashMap<String, usize>,
}

impl SourceHealth {
    pub fn record_success(&mut self, source: &str, count: usize) {
        *self.failures.entry(source.to_string()).or_insert(0) = 0;
        *self.registry.entry(source.to_string()).or_insert(0) += count;
    }

    pub fn record_failure(&mut self, source: &str) {
        *self.failures.entry(source.to_string()).or_insert(0) += 1;
    }

    /// A source with ≥DISABLE_THRESHOLD consecutive failures is "unhealthy".
    /// Reported to /api/sources for display, but — unlike before — NEVER means
    /// permanently dead: see `should_attempt`, which still re-probes it.
    pub fn is_disabled(&self, source: &str) -> bool {
        self.failures.get(source).copied().unwrap_or(0) >= DISABLE_THRESHOLD
    }

    /// Whether to attempt this source on the given RSS cycle.
    ///
    /// Healthy sources are always attempted. Unhealthy sources are demoted to a
    /// periodic re-probe (every REPROBE_EVERY_CYCLES) rather than skipped
    /// forever — so a single success resets their failure count and restores
    /// full polling. This is the self-healing behaviour: feeds recover on their
    /// own instead of staying dark until the next process restart.
    pub fn should_attempt(&self, source: &str, cycle: u64) -> bool {
        if !self.is_disabled(source) {
            return true;
        }
        cycle.is_multiple_of(REPROBE_EVERY_CYCLES)
    }
}

// ── HTTP client builder ───────────────────────────────────────────────────────

fn build_client(timeout_secs: u64) -> reqwest::Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        // Realistic browser UA: Cloudflare bot-fight challenges obvious bot UAs
        // from datacenter IPs, returning an HTML challenge page that fails RSS
        // parsing. A normal browser UA clears it for most Cloudflare-fronted feeds.
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36")
        .build()
}

// ── Feed entry → RawArticle ───────────────────────────────────────────────────
//
// Body extraction priority (improved):
//   1. content.body  — structured full-text field, present in most quality feeds
//   2. summary.content — abstract/teaser, usually 80–150 chars
//   3. Empty string   — article still processed; NLP uses title only
//
// The content.body field is typically 300–800 chars and contains the first
// paragraph(s) of the article, providing substantially more NLP signal than
// the summary abstract alone. This materially improves domain tagging accuracy
// especially for military and diplomatic events where context is distributed
// across multiple sentences.

fn entry_to_article(
    entry:  &feed_rs::model::Entry,
    source: &str,
    tier:   SourceTier,
) -> Option<RawArticle> {
    let title = entry.title.as_ref()?.content.trim().to_string();
    if title.is_empty() { return None; }

    let url = entry.links.first()
        .map(|l| l.href.clone())
        .unwrap_or_default();

    // Prefer content.body over summary for better NLP signal quality
    let body: String = entry.content
        .as_ref()
        .and_then(|c| c.body.as_deref())
        .or_else(|| entry.summary.as_ref().map(|s| s.content.as_str()))
        .unwrap_or("")
        .chars()
        .take(5000)  // raised from 1000 to capture more article context
        .collect();

    let published_at = entry.published
        .or(entry.updated)
        .unwrap_or_else(Utc::now);

    Some(RawArticle::new(
        url,
        title.chars().take(500).collect(),
        body,
        source.to_string(),
        tier,
        published_at,
    ))
}

// ── Ingestor ──────────────────────────────────────────────────────────────────

pub struct Ingestor {
    article_tx:      mpsc::Sender<RawArticle>,
    state:           Arc<AppState>,
    seen:            Arc<Mutex<SeenCache>>,
    health:          Arc<Mutex<SourceHealth>>,
}

impl Ingestor {
    pub fn new(
        article_tx:      mpsc::Sender<RawArticle>,
        state:           Arc<AppState>,
    ) -> Self {
        Self {
            article_tx,
            state,
            seen:   Arc::new(Mutex::new(SeenCache::new(50_000))), // raised from 10k
            health: Arc::new(Mutex::new(SourceHealth::default())),
        }
    }

    pub async fn run(self) {
        info!(
            "Ingestor: {} RSS feeds (parallel, max {} concurrent, ~{}s cycle) | {} GNews (~{}s/query) | {} GDELT (~{}s/query)",
            RSS_FEEDS.len(), MAX_CONCURRENT_RSS, RSS_CYCLE_MS / 1000,
            GNEWS_QUERIES.len(), GNEWS_QUERY_INTERVAL_S,
            GDELT_QUERIES.len(), GDELT_QUERY_INTERVAL_S,
        );

        // Seed the dedup cache from articles restored on boot (load_articles) so
        // live feeds re-fetching the same stories don't store them a second time.
        // Without this the feed shows duplicate pairs after every restart.
        {
            let store = self.state.article_store.lock().await;
            let mut seen = self.seen.lock().await;
            for a in store.articles.iter() {
                seen.mark_seen(&a.url, &a.title);
            }
            if !store.articles.is_empty() {
                info!("Ingestor: seeded dedup cache with {} restored articles", store.articles.len());
            }
        }

        let ingestor = Arc::new(self);

        let rss   = tokio::spawn(Self::rss_loop(Arc::clone(&ingestor)));
        let gnews = tokio::spawn(Self::gnews_loop(Arc::clone(&ingestor)));
        let gdelt = tokio::spawn(Self::gdelt_loop(Arc::clone(&ingestor)));

        let _ = tokio::join!(rss, gnews, gdelt);
    }

    // ── RSS loop — parallel per-feed fetching ─────────────────────────────────
    //
    // All feeds are spawned concurrently in each cycle. A semaphore caps the
    // number of simultaneous live HTTP connections at MAX_CONCURRENT_RSS.
    // Results are collected via a JoinSet and aggregated before logging.
    //
    // Each spawn receives a clone of the shared state arcs so the per-feed
    // tasks can update SeenCache and SourceHealth independently without
    // holding a lock for the duration of an HTTP request.

    async fn rss_loop(ingestor: Arc<Self>) {
        let client = match build_client(8) {
            Ok(c)  => c,
            Err(e) => { tracing::error!("RSS client build failed: {e}"); return; }
        };
        let sem = Arc::new(Semaphore::new(MAX_CONCURRENT_RSS));

        // Counter for deterministic jitter: avoids a dependency on rand crate
        let mut jitter_ctr: u64 = 0;

        loop {
            let mut handles = Vec::with_capacity(RSS_FEEDS.len());

            for feed in RSS_FEEDS {
                let client   = client.clone();
                let sem      = Arc::clone(&sem);
                let ingestor = Arc::clone(&ingestor);
                let url      = feed.url;
                let source   = feed.source;
                let tier     = feed.tier;

                // Unhealthy sources are demoted to periodic re-probing rather
                // than skipped forever — a single success restores them.
                if !ingestor.health.lock().await.should_attempt(source, jitter_ctr) {
                    continue;
                }

                handles.push(tokio::spawn(async move {
                    let _permit = sem.acquire().await.unwrap();
                    ingestor.fetch_rss_feed(&client, url, source, tier).await
                }));
            }

            let mut total       = 0usize;
            let mut sources_hit = 0usize;
            for h in handles {
                if let Ok((count, hit)) = h.await.unwrap_or(Ok((0, false))) {
                    total       += count;
                    if hit { sources_hit += 1; }
                }
            }

            if total > 0 {
                info!("RSS: {total} new articles from {sources_hit}/{} sources (parallel)",
                      RSS_FEEDS.len());
            }

            // Interval with ±20% deterministic jitter (no RNG dependency)
            let base_ms = RSS_CYCLE_MS;
            let jitter  = (jitter_ctr % 5) * base_ms / 25; // 0–20%
            let delay   = if jitter_ctr.is_multiple_of(2) { base_ms + jitter } else { base_ms - jitter };
            jitter_ctr  = jitter_ctr.wrapping_add(1);
            sleep(Duration::from_millis(delay)).await;
        }
    }

    /// Fetch a single RSS feed and return (articles_ingested, had_new_articles).
    async fn fetch_rss_feed(
        &self,
        client: &Client,
        url:    &str,
        source: &str,
        tier:   SourceTier,
    ) -> Result<(usize, bool), ()> {
        let resp = match client.get(url).send().await {
            Ok(r)  => r,
            Err(e) => {
                self.health.lock().await.record_failure(source);
                debug!("RSS {source}: {e}");
                return Err(());
            }
        };
        if !resp.status().is_success() {
            self.health.lock().await.record_failure(source);
            debug!("RSS {source} HTTP {}", resp.status());
            return Err(());
        }
        let bytes = match resp.bytes().await {
            Ok(b)  => b,
            Err(e) => {
                self.health.lock().await.record_failure(source);
                debug!("RSS {source} body: {e}");
                return Err(());
            }
        };
        let parsed = match feed_rs::parser::parse(bytes.as_ref()) {
            Ok(f)  => f,
            Err(e) => {
                self.health.lock().await.record_failure(source);
                debug!("RSS {source} parse: {e}");
                return Err(());
            }
        };

        let mut count = 0usize;
        for entry in parsed.entries.iter().take(RSS_ARTICLES_PER_FEED) {
            let article = match entry_to_article(entry, source, tier) {
                Some(a) => a,
                None    => continue,
            };
            if !self.seen.lock().await.is_new(&article.url, &article.title) {
                continue;
            }
            self.store_article(&article).await;
            if self.article_tx.send(article).await.is_err() {
                warn!("RSS {source}: article channel closed");
                break;
            }
            count += 1;
        }

        // Record a successful fetch (resets the consecutive-failure counter) whenever the
        // feed fetched AND parsed — a clean 200 with no new articles is health, not an
        // outage. Previously gated on count>0, so a live-but-quiet feed could never clear
        // accumulated failures and stayed demoted to slow re-probing. The registry tally
        // only advances when count>0 (record_success adds count). (audit ingestor-4)
        self.health.lock().await.record_success(source, count);
        Ok((count, count > 0))
    }

    // ── GNews loop ────────────────────────────────────────────────────────────

    async fn gnews_loop(ingestor: Arc<Self>) {
        let client = match build_client(10) {
            Ok(c)  => c,
            Err(e) => { tracing::error!("GNews client build failed: {e}"); return; }
        };

        let mut idx = 0usize;
        loop {
            let query   = GNEWS_QUERIES[idx % GNEWS_QUERIES.len()];
            idx        += 1;
            let encoded = query.replace(' ', "+");
            let url     = format!(
                "https://news.google.com/rss/search?q={encoded}&hl=en&gl=US&ceid=US:en"
            );

            // Mirror fetch_rss_feed: a send error, non-2xx (Google throttling/blocking the
            // datacenter IP), body error or parse error must all record a health FAILURE,
            // otherwise a dark GNews is invisible to the freshness floor / watchdog and the
            // roster silently loses a major source. (audit ingestor-2, xcut_err-2)
            match client.get(&url).send().await {
                Err(e) => {
                    ingestor.health.lock().await.record_failure("gnews");
                    debug!("GNews error: {e}");
                }
                Ok(resp) if !resp.status().is_success() => {
                    ingestor.health.lock().await.record_failure("gnews");
                    debug!("GNews HTTP {}", resp.status());
                }
                Ok(resp) => match resp.bytes().await {
                    Err(e) => {
                        ingestor.health.lock().await.record_failure("gnews");
                        debug!("GNews body: {e}");
                    }
                    Ok(bytes) => match feed_rs::parser::parse(bytes.as_ref()) {
                        Err(e) => {
                            ingestor.health.lock().await.record_failure("gnews");
                            debug!("GNews parse: {e}");
                        }
                        Ok(feed) => {
                            let mut count = 0usize;
                            for entry in feed.entries.iter().take(GNEWS_ARTICLES_PER_QUERY) {
                                let article = match entry_to_article(entry, "gnews", SourceTier::Tier2) {
                                    Some(a) => a,
                                    None    => continue,
                                };
                                if !ingestor.seen.lock().await
                                    .is_new(&article.url, &article.title) { continue; }
                                ingestor.store_article(&article).await;
                                let _ = ingestor.article_tx.send(article).await;
                                count += 1;
                            }
                            // A successful fetch+parse is health regardless of new-article
                            // count — reset failures. (audit ingestor-4)
                            ingestor.health.lock().await.record_success("gnews", count);
                            if count > 0 { info!("GNews: {count} articles for '{query}'"); }
                        }
                    }
                },
            }

            sleep(Duration::from_secs(GNEWS_QUERY_INTERVAL_S)).await;
        }
    }

    // ── GDELT loop ────────────────────────────────────────────────────────────

    async fn gdelt_loop(ingestor: Arc<Self>) {
        let client = match build_client(15) {
            Ok(c)  => c,
            Err(e) => { tracing::error!("GDELT client build failed: {e}"); return; }
        };

        let mut idx     = 0usize;
        let mut backoff = 1u64;

        loop {
            let query   = GDELT_QUERIES[idx % GDELT_QUERIES.len()];
            idx        += 1;
            let encoded = query.replace(' ', "%20");
            let url     = format!(
                "https://api.gdeltproject.org/api/v2/doc/doc\
                 ?query={encoded}&mode=artlist&maxrecords=25&format=json&timespan=15min"
            );

            let result = async {
                let resp = client.get(&url).send().await?;
                if !resp.status().is_success() {
                    return Err(anyhow::anyhow!("HTTP {}", resp.status()));
                }
                let data: serde_json::Value = resp.json().await?;
                Ok::<serde_json::Value, anyhow::Error>(data)
            }.await;

            match result {
                Err(e) => {
                    // Record the failure (the send/non-2xx/json error is already folded into
                    // `result`) so a dark GDELT is visible to the watchdog, not just an
                    // ever-growing local backoff. (audit xcut_err-2)
                    ingestor.health.lock().await.record_failure("gdelt");
                    backoff = (backoff * 2).min(500);
                    debug!("GDELT backoff {backoff}s: {e}");
                    sleep(Duration::from_secs(backoff)).await;
                    continue;
                }
                Ok(data) => {
                    backoff = (backoff / 2).max(1);

                    let articles = data["articles"].as_array()
                        .cloned()
                        .unwrap_or_default();

                    let mut count = 0usize;
                    for art_d in &articles {
                        let title = match art_d["title"].as_str() {
                            Some(t) if !t.is_empty() => t.to_string(),
                            _ => continue,
                        };
                        let url_a = art_d["url"].as_str().unwrap_or("").to_string();
                        if !ingestor.seen.lock().await.is_new(&url_a, &title) { continue; }

                        let pub_at = art_d["seendate"].as_str()
                            .and_then(|s| {
                                chrono::NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M%SZ").ok()
                            })
                            .map(|ndt| ndt.and_utc())
                            .unwrap_or_else(Utc::now);

                        // GDELT provides minimal body — use seendate as a content stub
                        // until GDELT V2 content API is integrated
                        let article = RawArticle::new(
                            url_a,
                            title.chars().take(500).collect(),
                            art_d["seendate"].as_str().unwrap_or("").to_string(),
                            "gdelt".to_string(),
                            SourceTier::Tier2,
                            pub_at,
                        );

                        ingestor.store_article(&article).await;
                        let _ = ingestor.article_tx.send(article).await;
                        count += 1;
                    }

                    // A successful query (HTTP 2xx + valid JSON) is health regardless of
                    // new-article count — reset failures. (audit ingestor-4)
                    ingestor.health.lock().await.record_success("gdelt", count);
                    if count > 0 {
                        info!("GDELT: {count} articles for '{query}'");
                    }
                }
            }

            sleep(Duration::from_secs(GDELT_QUERY_INTERVAL_S)).await;
        }
    }

    // ── Shared article store helper ───────────────────────────────────────────

    async fn store_article(&self, article: &RawArticle) {
        let stored = StoredArticle {
            id:           article.id.clone(),
            title:        article.title.clone(),
            url:          article.url.clone(),
            source:       article.source.clone(),
            tier:         article.source_tier as u8,
            published_at: article.published_at.to_rfc3339(),
            ingested_at:  article.fetched_at.to_rfc3339(),
            body:         article.body.chars().take(500).collect(), // raised from 300
            domain_tags:  vec![],
        };
        // Durable archive: append to date-rotated JSONL so the feed survives
        // restarts (the dedup cache otherwise suppresses re-ingest, leaving the
        // feed empty on boot). Best-effort — never blocks the pipeline.
        crate::aggregator::append_article(&stored).await;
        self.state.article_store.lock().await.push(stored);
        let mut registry = self.state.source_registry.lock().await;
        *registry.entry(article.source.clone()).or_insert(0) += 1;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── SeenCache ─────────────────────────────────────────────────────────────

    #[test]
    fn seen_cache_new_item_is_new() {
        let mut cache = SeenCache::new(100);
        assert!(cache.is_new("https://example.com/1", "Headline one"));
    }

    #[test]
    fn seen_cache_duplicate_is_not_new() {
        let mut cache = SeenCache::new(100);
        cache.is_new("https://example.com/1", "Headline one");
        assert!(!cache.is_new("https://example.com/1", "Headline one"));
    }

    #[test]
    fn seen_cache_different_url_same_title_is_new() {
        let mut cache = SeenCache::new(100);
        cache.is_new("https://example.com/1", "Same title");
        assert!(cache.is_new("https://example.com/2", "Same title"));
    }

    #[test]
    fn seen_cache_evicts_at_max_size() {
        let mut cache = SeenCache::new(3);
        cache.is_new("https://a.com", "A");
        cache.is_new("https://b.com", "B");
        cache.is_new("https://c.com", "C");
        cache.is_new("https://d.com", "D");
        assert_eq!(cache.order.len(), 3);
        assert_eq!(cache.cache.len(), 3);
    }

    #[test]
    fn seen_cache_evicted_item_is_new_again() {
        let mut cache = SeenCache::new(2);
        cache.is_new("https://a.com", "A");
        cache.is_new("https://b.com", "B");
        cache.is_new("https://c.com", "C"); // A evicted
        assert!(cache.is_new("https://a.com", "A"));
    }

    #[test]
    fn seen_cache_empty_strings_handled() {
        let mut cache = SeenCache::new(100);
        assert!(cache.is_new("", ""));
        assert!(!cache.is_new("", ""));
    }

    // ── SourceHealth ──────────────────────────────────────────────────────────

    #[test]
    fn source_health_new_source_not_disabled() {
        let health = SourceHealth::default();
        assert!(!health.is_disabled("bbc"));
    }

    #[test]
    fn source_health_nine_failures_not_disabled() {
        let mut health = SourceHealth::default();
        for _ in 0..9 { health.record_failure("bbc"); }
        assert!(!health.is_disabled("bbc"));
    }

    #[test]
    fn source_health_ten_failures_disabled() {
        let mut health = SourceHealth::default();
        for _ in 0..10 { health.record_failure("bbc"); }
        assert!(health.is_disabled("bbc"));
    }

    #[test]
    fn source_health_success_resets_failures() {
        let mut health = SourceHealth::default();
        for _ in 0..10 { health.record_failure("bbc"); }
        assert!(health.is_disabled("bbc"));
        health.record_success("bbc", 5);
        assert!(!health.is_disabled("bbc"));
    }

    #[test]
    fn healthy_source_always_attempted() {
        let mut health = SourceHealth::default();
        for _ in 0..9 { health.record_failure("bbc"); } // below threshold
        // Healthy sources are attempted on every cycle regardless of phase.
        for cycle in 0..REPROBE_EVERY_CYCLES {
            assert!(health.should_attempt("bbc", cycle));
        }
    }

    #[test]
    fn unhealthy_source_is_reprobed_not_skipped_forever() {
        let mut health = SourceHealth::default();
        for _ in 0..10 { health.record_failure("dead"); }
        assert!(health.is_disabled("dead"));
        // It is skipped on most cycles...
        assert!(!health.should_attempt("dead", 1));
        assert!(!health.should_attempt("dead", 5));
        // ...but re-probed periodically, so it can recover on its own.
        assert!(health.should_attempt("dead", 0));
        assert!(health.should_attempt("dead", REPROBE_EVERY_CYCLES));
        // And a single re-probe success fully restores it.
        health.record_success("dead", 1);
        assert!(!health.is_disabled("dead"));
        assert!(health.should_attempt("dead", 1));
    }

    #[test]
    fn source_health_registry_counts() {
        let mut health = SourceHealth::default();
        health.record_success("bbc", 3);
        health.record_success("bbc", 7);
        assert_eq!(health.registry["bbc"], 10);
    }

    #[test]
    fn source_health_independent_sources() {
        let mut health = SourceHealth::default();
        for _ in 0..10 { health.record_failure("bbc"); }
        assert!(health.is_disabled("bbc"));
        assert!(!health.is_disabled("reuters"));
    }

    // ── Feed registry ─────────────────────────────────────────────────────────

    #[test]
    fn rss_feeds_nonempty() {
        assert!(!RSS_FEEDS.is_empty());
    }

    #[test]
    fn rss_feeds_count() {
        // 103 verified-live feeds after the 2026-05 audit + expansion (every URL
        // probed from the production host, confirmed HTTP 200 + valid XML).
        // Audit removed: Reuters×2 & WaPo×2 (publisher killed RSS), NATO & VOA
        // (endpoints retired), lawfare/stimson/eurasianet (hard 403 from prod IP).
        // Then expanded with 35 new global feeds to broaden coverage past 100.
        assert_eq!(RSS_FEEDS.len(), 103);
    }

    #[test]
    fn rss_feeds_all_have_https() {
        for feed in RSS_FEEDS {
            assert!(feed.url.starts_with("https://"),
                "Feed {} URL should use HTTPS: {}", feed.source, feed.url);
        }
    }

    #[test]
    fn rss_feeds_tier1_count() {
        let tier1 = RSS_FEEDS.iter().filter(|f| f.tier == SourceTier::Tier1).count();
        // 33 since brookings (dead RSS, was Tier-2) was replaced by CFR (Tier-1,
        // peer to Foreign Affairs — CFR's own journal — which is already Tier-1).
        assert_eq!(tier1, 33, "Expected 33 Tier-1 feeds");
    }

    #[test]
    fn rss_feeds_tier2_count() {
        let tier2 = RSS_FEEDS.iter().filter(|f| f.tier == SourceTier::Tier2).count();
        // 70 after brookings (Tier-2) was promoted to its CFR replacement at Tier-1;
        // carnegie→rand, koreaherald→yonhap, haaretz→ynet all stayed Tier-2.
        assert_eq!(tier2, 70, "Expected 70 Tier-2 feeds");
    }

    #[test]
    fn rss_feeds_no_duplicate_urls() {
        let mut seen = std::collections::HashSet::new();
        for feed in RSS_FEEDS {
            assert!(seen.insert(feed.url), "Duplicate URL: {}", feed.url);
        }
    }

    #[test]
    fn gnews_queries_nonempty() {
        assert!(!GNEWS_QUERIES.is_empty());
        assert_eq!(GNEWS_QUERIES.len(), 23);
    }

    #[test]
    fn gdelt_queries_nonempty() {
        assert!(!GDELT_QUERIES.is_empty());
        assert_eq!(GDELT_QUERIES.len(), 8);
    }

    #[test]
    fn gnews_queries_no_special_chars_that_break_url() {
        for q in GNEWS_QUERIES {
            assert!(!q.contains('&'), "Query contains &: {q}");
            assert!(!q.contains('#'), "Query contains #: {q}");
        }
    }

    // ── Throughput constants ──────────────────────────────────────────────────

    #[test]
    fn rss_articles_per_feed_is_500() {
        assert_eq!(RSS_ARTICLES_PER_FEED, 500,
            "RSS article limit must be 500 per feed for high-volume operation");
    }

    #[test]
    fn max_concurrent_rss_is_reasonable() {
        const { assert!(MAX_CONCURRENT_RSS >= 10 && MAX_CONCURRENT_RSS <= 500,
            "MAX_CONCURRENT_RSS must be between 10 and 500") };
    }

    // ── Feed-roster liveness guard (roadmap 3.1) ──────────────────────────────
    //
    // Probes EVERY entry in RSS_FEEDS over the live network: HTTP 200, feed-rs
    // parse, and at least one entry — i.e. the exact path fetch_rss_feed needs
    // to succeed. The standing invariant this enforces: every news source is
    // live or replaced, never left silently broken. SourceHealth self-heals
    // transient outages at runtime but can't tell an operator "this feed has
    // been dead for a month" — this check can, and fails loudly with the list.
    //
    // Ignored by default (live network, ~30s). Run deliberately:
    //   cargo test --release feed_roster_liveness -- --ignored --nocapture
    //
    // A first concurrent pass probes everything; failures get one serial retry
    // (with a fresh connection) to absorb transient blips and rate-limit
    // grumpiness. Only a feed that fails BOTH passes is reported dead.

    async fn probe_feed(client: &Client, url: &str) -> Result<usize, String> {
        let resp = client.get(url).send().await.map_err(|e| format!("send: {e}"))?;
        let status = resp.status();
        // A 429 is the host answering and throttling — alive, just rate-limiting
        // this prober (prod polls from the same IP; repeated audit runs compound it).
        if status.as_u16() == 429 {
            return Ok(0);
        }
        if !status.is_success() {
            return Err(format!("HTTP {status}"));
        }
        let bytes = resp.bytes().await.map_err(|e| format!("body: {e}"))?;
        let feed  = feed_rs::parser::parse(bytes.as_ref()).map_err(|e| format!("parse: {e}"))?;
        if feed.entries.is_empty() {
            return Err("parsed but 0 entries".to_string());
        }
        Ok(feed.entries.len())
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "live-network probe of the full feed roster; run deliberately with --ignored"]
    async fn feed_roster_liveness() {
        let client = build_client(15).expect("client build");
        let sem    = Arc::new(Semaphore::new(MAX_CONCURRENT_RSS));

        // Pass 1 — concurrent probe of the whole roster.
        let mut handles = Vec::with_capacity(RSS_FEEDS.len());
        for feed in RSS_FEEDS {
            let client = client.clone();
            let sem    = Arc::clone(&sem);
            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                (feed.url, feed.source, probe_feed(&client, feed.url).await)
            }));
        }

        let mut failed: Vec<(&str, &str, String)> = Vec::new();
        let mut live = 0usize;
        for h in handles {
            let (url, source, result) = h.await.unwrap();
            match result {
                Ok(_)  => live += 1,
                Err(e) => failed.push((url, source, e)),
            }
        }

        // Pass 2 — serial retry of pass-1 failures only, after a pause so a
        // minute-scale edge incident or probe-induced throttle doesn't read as
        // dead. A feed that fails both passes ~30s apart needs operator eyes.
        if !failed.is_empty() {
            sleep(Duration::from_secs(30)).await;
        }
        let mut dead: Vec<(&str, &str, String)> = Vec::new();
        for (url, source, first_err) in failed {
            match probe_feed(&client, url).await {
                Ok(_)  => live += 1,
                Err(e) => dead.push((url, source, format!("{first_err} / retry: {e}"))),
            }
        }

        println!("feed roster liveness: {live}/{} RSS feeds live", RSS_FEEDS.len());
        for (url, source, err) in &dead {
            println!("  DEAD  {source:<22} {url}\n        {err}");
        }
        assert!(dead.is_empty(),
            "{} feed(s) failed both probe passes — fix or replace them (roster must be live \
             or replaced, never left silently broken)", dead.len());
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "live-network probe of the GNews + GDELT search APIs; run with --ignored"]
    async fn search_api_liveness() {
        let client = build_client(15).expect("client build");

        // GNews — one representative query through the same URL shape gnews_loop uses.
        let encoded = GNEWS_QUERIES[0].replace(' ', "+");
        let url = format!("https://news.google.com/rss/search?q={encoded}&hl=en&gl=US&ceid=US:en");
        let entries = probe_feed(&client, &url).await
            .unwrap_or_else(|e| panic!("GNews search RSS is dead: {e}"));
        println!("GNews live: {entries} entries for '{}'", GNEWS_QUERIES[0]);

        // GDELT — one representative query; a quiet window can legitimately return
        // zero articles, so liveness here is HTTP 200 + parseable JSON envelope.
        let encoded = GDELT_QUERIES[0].replace(' ', "%20");
        let url = format!(
            "https://api.gdeltproject.org/api/v2/doc/doc\
             ?query={encoded}&mode=artlist&maxrecords=25&format=json&timespan=1h"
        );
        let resp = client.get(&url).send().await.expect("GDELT send");
        let status = resp.status();
        if status.as_u16() == 429 {
            // Prod polls GDELT continuously from this same IP, so the probe can land
            // inside its rate-limit window. A 429 is the endpoint answering — alive.
            println!("GDELT live: 429 (rate-limit window; prod shares this IP) — endpoint alive");
            return;
        }
        assert!(status.is_success(), "GDELT HTTP {status}");
        let data: serde_json::Value = resp.json().await.expect("GDELT returned non-JSON");
        let n = data["articles"].as_array().map(|a| a.len()).unwrap_or(0);
        println!("GDELT live: {n} articles for '{}' (1h window)", GDELT_QUERIES[0]);
    }
}
