// ------------------------------------------------------------
// RAiTHE INDUSTRIES INCORPORATED
// Copyright (c) 2026 All Rights Reserved.
//
// This file is part of a proprietary system. Unauthorized use,
// reproduction, or distribution is strictly prohibited.
// ------------------------------------------------------------

// src/ingestor.rs — RSS / GNews / GDELT / video-transcript ingestion
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
//     A semaphore (MAX_CONCURRENT_RSS = 42) caps simultaneous HTTP connections
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
//   ARTICLE LIMITS (the real current constants are defined just below; the precise
//   throughput estimates that used to live here drifted badly from the code, so they were
//   dropped rather than left wrong — see RSS_ARTICLES_PER_FEED etc.):
//     RSS per-feed limit:    RSS_ARTICLES_PER_FEED    = 500 entries / poll.
//     GNews per-query limit: GNEWS_ARTICLES_PER_QUERY = 250 entries.
//     SeenCache capacity:    50,000 entries.
//     RSS roster re-polled on the ~100s RSS_CYCLE_MS cycle (NOT derived from the snapshot tick).
//
//   BACKOFF JITTER
//     A small deterministic jitter (±16% = (ctr%5)·base/25) is added to the RSS poll interval
//     to avoid thundering-herd effects across instances. Counter-based (no external RNG) for
//     reproducibility.

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
    // xinhuanet.com hard-403s its english RSS (probed 2026-07-03; Xinhua's newer
    // english.news.cn domain serves a stale 2018 shell). Replaced with ECNS — the
    // English service of China News Service, the other Chinese state wire: same
    // state-view purpose, probed 200 + valid RSS with same-day items.
    FeedSpec { url: "https://www.ecns.cn/rss/rss.xml",                              source: "ecns",            tier: SourceTier::Tier2 },
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
    // rnz's /rss/world.xml became a permanently EMPTY channel shell after a site
    // change (probed 2026-07-03: HTTP 200, valid RSS, zero <item>s — invisibly dead
    // to SourceHealth). Switched to the live Pacific feed: RNZ's distinctive beat
    // in this roster is the Pacific/NZ view, and pacific.xml carries same-day items.
    FeedSpec { url: "https://www.rnz.co.nz/rss/pacific.xml",                        source: "rnz",             tier: SourceTier::Tier2 },
    // Asia-Pacific — Tier 2
    FeedSpec { url: "https://www.channelnewsasia.com/api/v1/rss-outbound-feed?_format=xml", source: "cna",      tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.thehindu.com/news/international/feeder/default.rss",source: "thehindu",        tier: SourceTier::Tier2 },
    FeedSpec { url: "https://timesofindia.indiatimes.com/rssfeeds/296589292.cms",   source: "timesofindia",    tier: SourceTier::Tier2 },
    FeedSpec { url: "https://asiatimes.com/feed/",                                  source: "asiatimes",       tier: SourceTier::Tier2 },
    FeedSpec { url: "https://www.nknews.org/feed/",                                 source: "nknews",          tier: SourceTier::Tier2 },
    // globaltimes' RSS host stopped answering entirely (TCP connect timeout from
    // prod, probed 2026-07-03); replaced with CGTN World — the same Chinese
    // state-media view, probed 200 + valid RSS with current items.
    FeedSpec { url: "https://www.cgtn.com/subscribe/rss/section/world.xml",         source: "cgtn",            tier: SourceTier::Tier2 },
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

// ── URL canonicalization ──────────────────────────────────────────────────────
// Feeds decorate article links with per-fetch tracking params (BBC at_medium/
// at_campaign, SCMP utm_source, Guardian CMP, NYT smid, social fbclid/gclid) and
// fragments. None of them identify the article — they made SeenCache keys fragile
// (same story, "different" URL), left ~488 dirty outbound links in the live store,
// and would let a re-fetch with fresh params slip past dedup. (audit-news L3)

/// Query parameters that never identify an article — matched case-insensitively
/// against the param NAME; `utm_` is handled as a prefix (utm_source, utm_medium…).
const TRACKING_PARAM_NAMES: &[&str] =
    &["at_medium", "at_campaign", "cmp", "smid", "fbclid", "gclid", "ref"];

/// Canonicalize an article URL for dedup keys and storage: strip the `#fragment`,
/// drop tracking query params (any `utm_*` + TRACKING_PARAM_NAMES), and drop a
/// resulting bare trailing '?'. Non-tracking params (real article ids, pagination)
/// are preserved in their original order, so distinct articles stay distinct.
pub fn canonicalize_url(url: &str) -> String {
    let url = url.trim();
    // Fragment first — everything after '#' is client-side only.
    let no_frag = &url[..url.find('#').unwrap_or(url.len())];
    let Some((base, query)) = no_frag.split_once('?') else {
        return no_frag.to_string();
    };
    let is_tracking = |param: &str| {
        let name = param.split('=').next().unwrap_or(param).to_ascii_lowercase();
        name.starts_with("utm_") || TRACKING_PARAM_NAMES.contains(&name.as_str())
    };
    let kept: Vec<&str> = query.split('&')
        .filter(|p| !p.is_empty() && !is_tracking(p))
        .collect();
    if kept.is_empty() {
        base.to_string() // also drops a bare trailing '?' (SCMP)
    } else {
        format!("{base}?{}", kept.join("&"))
    }
}

// ── Seen cache — deduplication ────────────────────────────────────────────────
// MD5 of (canonical url + title) — same shape as the Python SeenCache.
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
        // Key on the CANONICAL url: tracking-param churn must not make the same
        // article look new, and archive-seeded keys (old rows carry raw URLs)
        // must match their live re-fetch either way.
        let input = format!("{}{title}", canonicalize_url(url));
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

    /// Non-marking membership probe: is this (url, title) already recorded? Unlike
    /// `is_new` this NEVER writes — callers that may abandon an item (the video loop
    /// probing before an expensive transcript fetch that can legitimately fail-and-
    /// retry) must not poison the cache for the item's later successful ingest or
    /// for an identically-titled wire article. (xhigh review findings 1+3)
    pub fn contains(&self, url: &str, title: &str) -> bool {
        self.cache.contains(&Self::key(url, title))
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

// ── Cross-feed title dedup ────────────────────────────────────────────────────
// The same story syndicates across feeds under different URLs (152 exact-duplicate
// titles live in an 11k store: one NATO headline carried verbatim by almonitor,
// thehindu AND straitstimes; gnews carrying publisher copies). SeenCache can't see
// those — its key includes the URL — so this second bounded FIFO set keys on the
// NORMALIZED title alone, across ALL sources. The event layer (FuzzyDedup) already
// suppressed the duplicate EVENTS; this stops the duplicate ARTICLES from piling
// into the visible feed. Deliberately EXACT-match after normalization — near-dup
// judgment stays with FuzzyDedup's Jaccard, which is untouched. (audit-news L1)

/// Normalize a title for exact cross-feed dedup: lowercase, punctuation → space,
/// whitespace collapsed — so case/punctuation variants of one headline match.
pub(crate) fn normalize_title_for_dedup(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut last_space = true;
    for c in title.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    while out.ends_with(' ') { out.pop(); }
    out
}

/// Below this normalized length a title is too generic to dedup across feeds
/// ("watch live", "in pictures" — different outlets legitimately reuse it for
/// DIFFERENT stories), so short titles bypass the cross-feed dedup entirely.
const TITLE_DEDUP_MIN_LEN: usize = 12;

/// How long a cross-feed title key blocks re-storage. A verbatim-recurring
/// headline separated by DAYS is a new edition/incident (weekly franchise
/// titles, "N. Korea fires ballistic missile toward East Sea"), not a
/// syndicated copy — only same-news-cycle repeats are duplicates.
const TITLE_DEDUP_TTL_S: i64 = 48 * 3600;

pub struct TitleDedup {
    cache:    std::collections::HashMap<String, i64>, // key → inserted-at (unix s)
    order:    VecDeque<String>,
    max_size: usize,
}

impl TitleDedup {
    pub fn new(max_size: usize) -> Self {
        Self {
            cache:    std::collections::HashMap::new(),
            order:    VecDeque::new(),
            max_size,
        }
    }

    /// md5 of the normalized title (fixed-size keys — memory bounded like
    /// SeenCache); None when the title is too short/generic to dedup safely.
    fn key(title: &str) -> Option<String> {
        let norm = normalize_title_for_dedup(title);
        if norm.len() < TITLE_DEDUP_MIN_LEN { return None; }
        Some(format!("{:x}", md5::compute(norm.as_bytes())))
    }

    /// True if no article with this normalized title has been stored before —
    /// from ANY source — within the TTL window. Records/refreshes the title.
    /// Short/generic titles always pass.
    pub fn is_new(&mut self, title: &str) -> bool {
        self.is_new_at(title, chrono::Utc::now().timestamp())
    }

    fn is_new_at(&mut self, title: &str, now_s: i64) -> bool {
        let Some(k) = Self::key(title) else { return true; };
        if let Some(&at) = self.cache.get(&k) {
            if now_s - at <= TITLE_DEDUP_TTL_S {
                return false;
            }
            // Expired: this is a NEW edition of a recurring headline. Refresh the
            // timestamp in place (the stale order slot evicts early later — that
            // only shortens dedup memory, never hides a fresh story).
            self.cache.insert(k, now_s);
            return true;
        }
        self.cache.insert(k.clone(), now_s);
        self.order.push_back(k);
        if self.order.len() > self.max_size {
            if let Some(old) = self.order.pop_front() {
                self.cache.remove(&old);
            }
        }
        true
    }

    /// Non-marking membership probe (TTL-aware): is this title currently deduped?
    /// Never writes — see SeenCache::contains for why the video loop needs this.
    pub fn contains(&self, title: &str) -> bool {
        let Some(k) = Self::key(title) else { return false; };
        match self.cache.get(&k) {
            Some(&at) => chrono::Utc::now().timestamp() - at <= TITLE_DEDUP_TTL_S,
            None => false,
        }
    }

    /// Boot seeding: mark a title as already stored without reporting novelty.
    pub fn mark_seen(&mut self, title: &str) {
        let Some(k) = Self::key(title) else { return; };
        if self.cache.insert(k.clone(), chrono::Utc::now().timestamp()).is_none() {
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

/// Hard ceiling on a single feed response body. Even with a client timeout, a misbehaving or
/// compromised upstream could stream gigabytes within the time budget; this bounds memory.
/// 16 MB is generous for RSS/Atom/JSON feeds. (audit ingestor-3 / xcut_net-5)
const MAX_FEED_BODY_BYTES: u64 = 16 * 1024 * 1024;

/// Read a response body with a hard byte ceiling. Rejects a declared-oversize up front via
/// Content-Length, and otherwise accumulates chunks, aborting past the cap — so a chunked
/// response with no Content-Length is bounded too.
async fn read_body_capped(resp: reqwest::Response, cap: u64) -> Result<bytes::Bytes, String> {
    if let Some(len) = resp.content_length() {
        if len > cap {
            return Err(format!("body too large: {len} bytes > {cap} cap"));
        }
    }
    let mut total: u64 = 0;
    let mut buf = bytes::BytesMut::new();
    let mut resp = resp;
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                total += chunk.len() as u64;
                if total > cap {
                    return Err(format!("body exceeded {cap} byte cap"));
                }
                buf.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(e) => return Err(format!("body: {e}")),
        }
    }
    Ok(buf.freeze())
}

// ── Title / body hygiene ──────────────────────────────────────────────────────
// Feeds ship HTML entities in titles (thecradle "&#039;", timesofindia "&amp;"),
// raw markup in bodies (1,868/11,000 live articles; middleeastmonitor opens with
// ~300 chars of pure <div><img> furniture, which then IS the LLM's 600-byte
// excerpt), and CMS boilerplate tails. No new crate dependencies — a small
// explicit scrubber for what feeds actually emit. (audit-news C1–C4)

/// Strip the " - <Outlet>" attribution Google News appends to every headline
/// ("NATO summit opens - Reuters"). Applied to gnews entries BEFORE dedup keys and
/// storage, so a story's gnews copy matches its publisher-feed copy (and the
/// "- Euronews" / "- Euronews.com" / "- Reuters" variants of ONE story collapse).
/// Only the LAST " - " segment is dropped, and only when a substantial title
/// remains, so a short hyphenated clause isn't butchered.
pub(crate) fn strip_gnews_outlet_suffix(title: &str) -> &str {
    match title.rfind(" - ") {
        Some(i) if title[..i].trim_end().len() >= 10 => title[..i].trim_end(),
        _ => title,
    }
}

/// Named HTML entities that actually occur in feed titles/bodies. Deliberately a
/// small explicit set, not a spec implementation — unknown entities stay visible
/// rather than being guessed at. Numeric forms are decoded separately.
const NAMED_ENTITIES: &[(&str, &str)] = &[
    ("&amp;", "&"), ("&lt;", "<"), ("&gt;", ">"), ("&quot;", "\""), ("&apos;", "'"),
    ("&nbsp;", " "), ("&ndash;", "\u{2013}"), ("&mdash;", "\u{2014}"),
    ("&lsquo;", "\u{2018}"), ("&rsquo;", "\u{2019}"),
    ("&ldquo;", "\u{201c}"), ("&rdquo;", "\u{201d}"), ("&hellip;", "\u{2026}"),
];

/// Decode common named + numeric (`&#039;` decimal, `&#x27;` hex) HTML entities.
/// Unrecognised ampersand runs are kept literally — visible beats silently wrong.
fn decode_html_entities(s: &str) -> String {
    if !s.contains('&') { return s.to_string(); }
    let mut out  = String::with_capacity(s.len());
    let mut rest = s;
    'outer: while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        // An entity's ';' arrives within a short window (real entities are short).
        // Byte-scan for the ASCII ';' — a fixed-width str slice here could split a
        // multibyte char right after the '&' and panic.
        if let Some(semi) = tail.bytes().take(12).position(|b| b == b';') {
            let ent = &tail[..=semi];
            if let Some(num) = ent.strip_prefix("&#").and_then(|e| e.strip_suffix(';')) {
                let parsed = if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                    u32::from_str_radix(hex, 16).ok()
                } else {
                    num.parse::<u32>().ok()
                };
                if let Some(c) = parsed.and_then(char::from_u32) {
                    out.push(c);
                    rest = &tail[semi + 1..];
                    continue 'outer;
                }
            }
            if let Some((_, repl)) = NAMED_ENTITIES.iter().find(|(name, _)| *name == ent) {
                out.push_str(repl);
                rest = &tail[semi + 1..];
                continue 'outer;
            }
        }
        // Not a recognisable entity — keep the '&' literally and move on.
        out.push('&');
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

/// Strip HTML tags: every `<…>` span (with its attributes) becomes a single space
/// so `</p><p>` doesn't glue words together (whitespace is collapsed afterwards).
/// An unterminated '<' (truncated feed body) drops the remainder — markup must
/// never leak into the store.
fn strip_html_tags(s: &str) -> String {
    if !s.contains('<') { return s.to_string(); }
    let mut out    = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match (in_tag, c) {
            (false, '<') => in_tag = true,
            (false, _)   => out.push(c),
            (true, '>')  => { in_tag = false; out.push(' '); }
            (true, _)    => {}
        }
    }
    out
}

/// Cut CMS boilerplate tails: the WordPress "The post <title> appeared first on
/// <outlet>." footer (timesofisrael et al) and Guardian's "Continue reading...".
/// Applied BEFORE truncation so the stored excerpt is article text, not furniture.
fn strip_boilerplate_tail(s: &str) -> &str {
    let mut end = s.len();
    if let Some(i) = s.find("Continue reading") { end = end.min(i); }
    if let Some(j) = s.find("appeared first on") {
        // Cut from the "The post " that opens the footer sentence when present,
        // otherwise from the phrase itself — never from an earlier unrelated
        // "The post office…" in the article body.
        end = end.min(s[..j].rfind("The post ").unwrap_or(j));
    }
    s[..end].trim_end()
}

/// Feed-text scrubber for titles and bodies: strip tags, decode entities, cut
/// boilerplate tails, collapse whitespace.
pub(crate) fn sanitize_feed_text(s: &str) -> String {
    let stripped = strip_html_tags(s);
    let decoded  = decode_html_entities(&stripped);
    let cut      = strip_boilerplate_tail(&decoded);
    let joined   = cut.split_whitespace().collect::<Vec<_>>().join(" ");
    // feed-rs 2.x's own sanitizer eats the '&' of a DOUBLE-encoded ampersand
    // (&amp;amp; → "amp;") before this code ever sees the text — repair the
    // orphaned entity tail. " amp; " is not a token legitimate headlines produce.
    joined.replace(" amp; ", " & ")
}

/// The exact title form the ingest path keys dedup on: sanitized, and for gnews
/// with the " - Outlet" suffix stripped. Boot seeding MUST derive keys through
/// this same function or archived rows (stored raw, before this hygiene existed)
/// won't match their own live re-fetch and get stored twice.
fn ingest_title_key(source: &str, raw_title: &str) -> String {
    let t = sanitize_feed_text(raw_title);
    if source == "gnews" { strip_gnews_outlet_suffix(&t).to_string() } else { t }
}

// ── Junk filter ───────────────────────────────────────────────────────────────
// Two conservative classes only. (1) Recurring wire furniture that carries no
// event (yonhap's daily "Yonhap News Summary" digest, taipeitimes' "EDITORIAL
// CARTOON"). (2) Per-feed sports SECTIONS by URL path (live at audit time: 76 bbc
// /sport/ + 38 guardian /sport/+/football/ articles in the store). Path-based
// ONLY — never keyword-based — so a geopolitics story can never be dropped for
// how its headline reads. (audit-news L6 + off-topic flood)

const JUNK_TITLE_PREFIXES: &[&str] = &["yonhap news summary", "editorial cartoon"];

/// Per-source sports-section URL paths to exclude. The source match is exact and
/// each path substring includes the host, so e.g. a guardian story ABOUT football
/// politics under /world/ is untouched.
fn is_excluded_path(source: &str, url: &str) -> bool {
    match source {
        "bbc"      => url.contains("bbc.co.uk/sport/") || url.contains("bbc.com/sport/"),
        "guardian" => url.contains("theguardian.com/sport/")
                   || url.contains("theguardian.com/football/"),
        "abc_au"   => url.contains("abc.net.au/sport/") || url.contains("/news/sport/"),
        _          => false,
    }
}

/// Junk gate applied at ingest, before dedup keys and storage.
fn is_junk_entry(source: &str, url: &str, title: &str) -> bool {
    let tl = title.trim().to_lowercase();
    JUNK_TITLE_PREFIXES.iter().any(|p| tl.starts_with(p)) || is_excluded_path(source, url)
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

/// Parse a feed WITHOUT feed-rs 2.x's built-in content sanitizer: it mangles
/// ENCODED markup destructively ("&lt;p&gt;text" → "ptext", "&amp;amp;" → "amp;")
/// before this module's own sanitation — which handles those shapes correctly —
/// ever sees the text. Parity with the feed-rs 1.x raw-content behavior.
pub(crate) fn parse_feed_raw(bytes: &[u8]) -> Result<feed_rs::model::Feed, feed_rs::parser::ParseFeedError> {
    feed_rs::parser::Builder::new().sanitize_content(false).build().parse(bytes)
}

fn entry_to_article(
    entry:  &feed_rs::model::Entry,
    source: &str,
    tier:   SourceTier,
) -> Option<RawArticle> {
    let raw_title = entry.title.as_ref()?.content.trim().to_string();
    if raw_title.is_empty() { return None; }
    // skynews live-blog junk: the "title" is literally an "<a href=…>…" tag —
    // nothing meaningful survives sanitation, so reject the entry outright.
    // feed-rs 2.x strips the angle brackets but leaves the tag guts inline
    // ("a href='…'Headline"), so match that mangled shape too.
    if raw_title.starts_with('<') || raw_title.starts_with("a href=") { return None; }

    let mut title: String = sanitize_feed_text(&raw_title).chars().take(500).collect();
    if source == "gnews" {
        title = strip_gnews_outlet_suffix(&title).to_string();
    }
    if title.is_empty() { return None; }

    let url = canonicalize_url(
        &entry.links.first().map(|l| l.href.clone()).unwrap_or_default());

    if is_junk_entry(source, &url, &title) { return None; }

    // Prefer content.body over summary for better NLP signal quality. Sanitize
    // (tags out, entities decoded, boilerplate tails cut) BEFORE truncating so
    // the stored excerpt and the LLM's 600-byte window hold text, not markup.
    let raw_body: String = entry.content
        .as_ref()
        .and_then(|c| c.body.as_deref())
        .or_else(|| entry.summary.as_ref().map(|s| s.content.as_str()))
        .unwrap_or("")
        .chars()
        .take(20_000) // bound the scrubber's work; far past any useful excerpt
        .collect();
    let body: String = sanitize_feed_text(&raw_body)
        .chars()
        .take(5000)  // raised from 1000 to capture more article context
        .collect();

    let published_at = entry.published
        .or(entry.updated)
        .unwrap_or_else(Utc::now);

    Some(RawArticle::new(
        url,
        title,
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
    titles:          Arc<Mutex<TitleDedup>>,
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
            titles: Arc::new(Mutex::new(TitleDedup::new(50_000))),
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

        // Seed the dedup caches from the archive so live feeds re-fetching the same
        // stories don't store them a second time (neither cache is itself persisted).
        // Two layers: a KEYS-ONLY replay of ~5 further archive days (slow feeds —
        // think-tanks, journals — keep entries live for a week+, so a cache seeded
        // with only two days re-stored anything older after every restart,
        // audit-news L5), then the in-memory store (today+yesterday, restored by
        // load_articles). Oldest first, so FIFO eviction keeps the newest keys.
        {
            let mut seen   = self.seen.lock().await;
            let mut titles = self.titles.lock().await;
            let archived   = crate::aggregator::load_archived_article_keys(6, 2).await;
            for (url, title, source) in &archived {
                // Stored titles already went through ingest hygiene, so re-deriving the
                // key can over-strip (a gnews title with a legitimate final " - segment"
                // loses it a second time). Seed BOTH forms: the as-stored title and the
                // re-derived key — whichever the live re-fetch produces, it matches.
                let as_stored = sanitize_feed_text(title);
                let rederived = ingest_title_key(source, title);
                seen.mark_seen(url, &as_stored);
                titles.mark_seen(&as_stored);
                if rederived != as_stored {
                    seen.mark_seen(url, &rederived);
                    titles.mark_seen(&rederived);
                }
            }
            let store = self.state.article_store.lock().await;
            for a in store.articles.iter() {
                let as_stored = sanitize_feed_text(&a.title);
                let rederived = ingest_title_key(&a.source, &a.title);
                seen.mark_seen(&a.url, &as_stored);
                titles.mark_seen(&as_stored);
                if rederived != as_stored {
                    seen.mark_seen(&a.url, &rederived);
                    titles.mark_seen(&rederived);
                }
            }
            if !store.articles.is_empty() || !archived.is_empty() {
                info!("Ingestor: seeded dedup caches with {} restored + {} archived article keys",
                      store.articles.len(), archived.len());
            }
        }

        let ingestor = Arc::new(self);

        let rss   = tokio::spawn(Self::rss_loop(Arc::clone(&ingestor)));
        let gnews = tokio::spawn(Self::gnews_loop(Arc::clone(&ingestor)));
        let gdelt = tokio::spawn(Self::gdelt_loop(Arc::clone(&ingestor)));
        let video = tokio::spawn(Self::video_loop(Arc::clone(&ingestor)));
        let live  = tokio::spawn(Self::livestream_loop(Arc::clone(&ingestor)));

        let _ = tokio::join!(rss, gnews, gdelt, video, live);
    }

    // ── Video loop — YouTube channel transcripts as articles (src/video.rs) ───
    //
    // DORMANT unless GCRM_VIDEO_SOURCES=1: the loop exits immediately when the
    // operator has not opted in, so shipping this costs prod nothing. When live,
    // each cycle polls the watchlist channels' Atom feeds, pulls captions for
    // NEW recent uploads via a local yt-dlp (subtitles only), and feeds the
    // flattened transcript through the exact article path wire copy uses —
    // same dedup, same NLP, same enricher, same store, same dashboard row
    // (title → YouTube link, channel as source, upload time as timestamp).
    async fn video_loop(ing: Arc<Self>) {
        use crate::video;
        if !video::enabled() {
            info!("Video sources: dormant (set GCRM_VIDEO_SOURCES=1 to enable {} channels)",
                  video::VIDEO_CHANNELS.len());
            return;
        }
        let client = match build_client(20) {
            Ok(c) => c,
            Err(e) => { warn!("Video sources: client build failed: {e}"); return; }
        };
        info!("Video sources: LIVE — {} channels, {}s cycle, yt-dlp at {}",
              video::VIDEO_CHANNELS.len(), video::VIDEO_POLL_SECS,
              video::ytdlp_bin().display());
        // Videos already transcribed (or aged out) this process lifetime; the
        // article-store URL dedup covers restarts. A no-caption video is NOT
        // marked done, so it retries each cycle until captions appear or the
        // age gate closes over it.
        let mut done: std::collections::HashSet<String> = std::collections::HashSet::new();
        loop {
            for ch in video::VIDEO_CHANNELS {
                let feed_url = video::channel_feed_url(ch.channel_id);
                let bytes = match client.get(&feed_url).send().await {
                    Ok(r) if r.status().is_success() => match read_body_capped(r, MAX_FEED_BODY_BYTES).await {
                        Ok(b) => b,
                        Err(e) => { debug!("video {}: body: {e}", ch.source); continue; }
                    },
                    Ok(r) => { debug!("video {}: HTTP {}", ch.source, r.status()); continue; }
                    Err(e) => { debug!("video {}: {e}", ch.source); continue; }
                };
                let parsed = match parse_feed_raw(bytes.as_ref()) {
                    Ok(f) => f,
                    Err(e) => { debug!("video {}: parse: {e}", ch.source); continue; }
                };
                let mut attempted = 0usize;
                for entry in parsed.entries.iter() {
                    // Cap ATTEMPTS (yt-dlp subprocess launches), not successes: a
                    // livestream-clip flood of uncaptioned uploads would otherwise run
                    // unbounded 90s subprocesses and starve the other channels — the
                    // exact scenario the cap exists for. (xhigh review finding 15)
                    if attempted >= video::VIDEOS_PER_CHANNEL_PER_CYCLE { break; }
                    let Some(title) = entry.title.as_ref().map(|t| {
                        video::strip_channel_suffix(&sanitize_feed_text(t.content.trim())).to_string()
                    }) else { continue };
                    if title.is_empty() { continue; }
                    let url = canonicalize_url(
                        &entry.links.first().map(|l| l.href.clone()).unwrap_or_default());
                    if url.is_empty() || done.contains(&url) { continue; }
                    // Shorts: sub-minute clips/teasers — near-zero transcript value,
                    // pure feed clutter. The full story arrives as a normal upload.
                    if video::is_short(&url) { done.insert(url); continue; }
                    let published = match entry.published.or(entry.updated) {
                        Some(p) => p,
                        None => continue,
                    };
                    let age = chrono::Utc::now() - published;
                    if age > chrono::Duration::hours(video::VIDEO_MAX_AGE_HOURS) {
                        done.insert(url); // aged out — never transcribe
                        continue;
                    }
                    // Dedup probe BEFORE the expensive transcript fetch — via the
                    // NON-MARKING `contains`, never `is_new`: marking here poisoned
                    // both caches for any video that then failed to ingest (no
                    // captions yet / off-mission / fetch error), which (a) killed the
                    // designed retry-until-captioned — the next cycle read the mark
                    // as "duplicate" and permanently done-marked the upload — and
                    // (b) suppressed an identically-titled REAL wire article for the
                    // dedup TTL, a story then existing in no form in the feed.
                    // Marks are recorded only at successful store below.
                    // (xhigh review findings 1+3)
                    if ing.seen.lock().await.contains(&url, &title) { done.insert(url); continue; }
                    if ing.titles.lock().await.contains(&title) { done.insert(url); continue; }
                    attempted += 1;
                    match video::fetch_transcript(&url).await {
                        Ok(Some(raw_transcript)) => {
                            // Signal-dense condensation: pack the NLP/enricher budget
                            // with trigger-bearing sentences, not the first N minutes.
                            let transcript = video::condense_transcript(&raw_transcript, video::TRANSCRIPT_MAX_CHARS);
                            // Relevance gate: broadcast channels mix missions — sports,
                            // royals, celebrations. Keep a video only when the TITLE or
                            // the TRANSCRIPT carries a geopolitical trigger (actors +
                            // conflict terms — the same gate that dispatches the LLM on
                            // keyword-missed wire copy). Deliberately BROAD, not the
                            // domain-keyword lexicon: the proven-valuable analyst
                            // register ("the strait is not open") scores zero keywords
                            // but names its actors, and a false keep costs one harmless
                            // untagged row while a false drop loses real signal.
                            if !crate::nlp_sidecar::has_geopolitical_trigger(&title)
                                && !crate::nlp_sidecar::has_geopolitical_trigger(&transcript)
                            {
                                debug!("video {}: off-mission, skipped \"{}\"", ch.source,
                                       title.chars().take(60).collect::<String>());
                                done.insert(url);
                                continue;
                            }
                            let article = RawArticle::new(
                                url.clone(), title.clone(), transcript,
                                ch.source.to_string(), ch.tier, published.with_timezone(&chrono::Utc),
                            );
                            // Successful ingest: NOW record the dedup marks.
                            let _ = ing.seen.lock().await.is_new(&article.url, &article.title);
                            let _ = ing.titles.lock().await.is_new(&article.title);
                            // Labeled-pair collection (roadmap: cross-modal merge threshold):
                            // log video↔wire title pairs in the ambiguous similarity band to
                            // logs/video-pairs-<date>.jsonl for the operator's later labeling.
                            // Data collection only — never feeds the model.
                            {
                                let store = ing.state.article_store.lock().await;
                                let mut pairs: Vec<String> = Vec::new();
                                for w in store.query(400, None, None) {
                                    if w.source.ends_with("-video") || w.source.ends_with("-live") { continue; }
                                    let s = video::title_trigram_jaccard(&article.title, &w.title);
                                    if s >= video::PAIR_LOG_BAND.0 && s <= video::PAIR_LOG_BAND.1 {
                                        pairs.push(serde_json::json!({
                                            "t": chrono::Utc::now().to_rfc3339(),
                                            "sim": (s * 1e3).round() / 1e3,
                                            "video_source": article.source, "video_title": article.title,
                                            "wire_source": w.source, "wire_title": w.title,
                                        }).to_string());
                                    }
                                }
                                drop(store);
                                if !pairs.is_empty() {
                                    let path = format!("logs/video-pairs-{}.jsonl", chrono::Utc::now().format("%Y-%m-%d"));
                                    if let Ok(mut f) = tokio::fs::OpenOptions::new().create(true).append(true).open(&path).await {
                                        use tokio::io::AsyncWriteExt;
                                        let _ = f.write_all((pairs.join("\n") + "\n").as_bytes()).await;
                                    }
                                }
                            }
                            ing.store_article(&article).await;
                            if ing.article_tx.send(article).await.is_err() {
                                warn!("video {}: article channel closed", ch.source);
                                return;
                            }
                            done.insert(url);
                            info!("video {}: ingested \"{}\"", ch.source, title.chars().take(80).collect::<String>());
                        }
                        Ok(None) => {
                            debug!("video {}: no captions yet for \"{}\"", ch.source, title.chars().take(60).collect::<String>());
                        }
                        Err(e) => {
                            warn!("video {}: transcript fetch failed: {e}", ch.source);
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(video::VIDEO_POLL_SECS)).await;
        }
    }

    // ── Live-stream loop — rolling broadcast transcripts (src/livestream.rs) ──
    //
    // DORMANT unless GCRM_LIVESTREAM_SOURCES=1. One article per stream (the plain
    // watch page URL), UPDATED in place per transcribed window via the store's
    // live-blog path — a rolling "what the channel is saying right now" row; every
    // window still flows through NLP/enricher as fresh evidence. Relevance-gated
    // like video: a window with no geopolitical trigger is skipped, not stored.
    async fn livestream_loop(ing: Arc<Self>) {
        use crate::livestream;
        if !livestream::enabled() {
            info!("Live-stream sources: dormant (set GCRM_LIVESTREAM_SOURCES=1 to enable {} streams)",
                  livestream::LIVE_STREAMS.len());
            return;
        }
        info!("Live-stream sources: LIVE — {} streams, {}s cycle, whisper at {}",
              livestream::LIVE_STREAMS.len(), livestream::LIVESTREAM_POLL_SECS,
              livestream::whisper_bin().display());
        loop {
            for st in livestream::LIVE_STREAMS {
                match livestream::capture_transcript(st.page).await {
                    Ok(Some(win)) => {
                        if !crate::nlp_sidecar::has_geopolitical_trigger(&win) {
                            debug!("live {}: window off-mission, skipped", st.source);
                            continue;
                        }
                        let label = st.source.trim_end_matches("-live");
                        let title = livestream::live_title(label, &win);
                        let body  = crate::video::condense_transcript(&win, crate::video::TRANSCRIPT_MAX_CHARS);
                        let article = RawArticle::new(
                            st.page.to_string(), title.clone(), body,
                            st.source.to_string(), st.tier, chrono::Utc::now(),
                        );
                        let _ = ing.seen.lock().await.is_new(&article.url, &article.title);
                        let _ = ing.titles.lock().await.is_new(&article.title);
                        ing.store_article(&article).await; // same URL → update-in-place
                        if ing.article_tx.send(article).await.is_err() {
                            warn!("live {}: article channel closed", st.source);
                            return;
                        }
                        info!("live {}: window ingested \"{}\"", st.source,
                              title.chars().take(90).collect::<String>());
                    }
                    Ok(None) => debug!("live {}: not streaming / empty window", st.source),
                    Err(e) => warn!("live {}: {e}", st.source),
                }
            }
            tokio::time::sleep(Duration::from_secs(livestream::LIVESTREAM_POLL_SECS)).await;
        }
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

                // Carry the source id with the handle so a task PANIC (not just a fetch
                // error) can be attributed and health-recorded below. (audit xcut_net-6)
                handles.push((source, tokio::spawn(async move {
                    let _permit = sem.acquire().await.unwrap();
                    ingestor.fetch_rss_feed(&client, url, source, tier).await
                })));
            }

            let mut total       = 0usize;
            let mut sources_hit = 0usize;
            for (source, h) in handles {
                match h.await {
                    Ok(Ok((count, hit))) => {
                        total += count;
                        if hit { sources_hit += 1; }
                    }
                    // fetch_rss_feed already recorded the failure for a transport/parse error.
                    Ok(Err(())) => {}
                    // The per-feed task itself panicked (e.g. a parser panic on malformed
                    // provider input). Record a health failure so should_attempt demotes it
                    // like a network failure instead of it vanishing silently — and the
                    // watchdog can see it. (audit xcut_net-6)
                    Err(join_err) => {
                        ingestor.health.lock().await.record_failure(source);
                        warn!("RSS {source}: task panicked: {join_err}");
                    }
                }
            }

            if total > 0 {
                info!("RSS: {total} new articles from {sources_hit}/{} sources (parallel)",
                      RSS_FEEDS.len());
            }

            // Interval with ±16% deterministic jitter (no RNG dependency): (ctr%5)∈0..4, /25 → max 4/25 = 16%
            let base_ms = RSS_CYCLE_MS;
            let jitter  = (jitter_ctr % 5) * base_ms / 25; // 0–16%
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
        let bytes = match read_body_capped(resp, MAX_FEED_BODY_BYTES).await {
            Ok(b)  => b,
            Err(e) => {
                self.health.lock().await.record_failure(source);
                debug!("RSS {source} body: {e}");
                return Err(());
            }
        };
        let parsed = match parse_feed_raw(bytes.as_ref()) {
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
            // Cross-feed exact-title dedup: an identical normalized headline already
            // stored from ANY source is the same story syndicated — drop it. A
            // live-blog EDIT (same URL, new title) passes here and becomes an
            // update-in-place inside store_article. (audit-news L1)
            if !self.titles.lock().await.is_new(&article.title) {
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
                    ingestor.note_search_api("gnews", Some(format!("send: {e}"))).await;
                    debug!("GNews error: {e}");
                }
                Ok(resp) if !resp.status().is_success() => {
                    let status = resp.status();
                    ingestor.health.lock().await.record_failure("gnews");
                    ingestor.note_search_api("gnews", Some(format!("HTTP {status}"))).await;
                    debug!("GNews HTTP {status}");
                }
                Ok(resp) => match read_body_capped(resp, MAX_FEED_BODY_BYTES).await {
                    Err(e) => {
                        ingestor.health.lock().await.record_failure("gnews");
                        ingestor.note_search_api("gnews", Some(format!("body: {e}"))).await;
                        debug!("GNews body: {e}");
                    }
                    Ok(bytes) => match parse_feed_raw(bytes.as_ref()) {
                        Err(e) => {
                            ingestor.health.lock().await.record_failure("gnews");
                            ingestor.note_search_api("gnews", Some(format!("parse: {e}"))).await;
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
                                // Cross-feed title dedup — gnews is the roster's largest
                                // duplicate injector (publisher copies + per-outlet
                                // "- X" variants of one story). (audit-news L4)
                                if !ingestor.titles.lock().await
                                    .is_new(&article.title) { continue; }
                                ingestor.store_article(&article).await;
                                let _ = ingestor.article_tx.send(article).await;
                                count += 1;
                            }
                            // A successful fetch+parse is health regardless of new-article
                            // count — reset failures. (audit ingestor-4)
                            ingestor.health.lock().await.record_success("gnews", count);
                            ingestor.note_search_api("gnews", None).await;
                            if count > 0 { info!("GNews: {count} articles for '{query}'"); }
                        }
                    }
                },
            }

            sleep(Duration::from_secs(GNEWS_QUERY_INTERVAL_S)).await;
        }
    }

    // ── GDELT loop ────────────────────────────────────────────────────────────

    /// Floor on the GDELT failure-retry delay, in seconds. GDELT's 429 body
    /// documents a hard "one request every 5 seconds" per-IP limit; the old
    /// backoff started at 2s, so after any 429 the retry itself violated the
    /// limit and GUARANTEED another 429 — a self-sustaining throttle loop that
    /// kept GDELT dark (0 stored articles over days, live-diagnosed 2026-07-03).
    /// Comfortably above the documented limit so a throttle window can drain.
    const GDELT_MIN_RETRY_S: u64 = 30;

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
                // Reject a declared-oversize body before buffering it as JSON. (audit ingestor-3)
                if resp.content_length().is_some_and(|l| l > MAX_FEED_BODY_BYTES) {
                    return Err(anyhow::anyhow!("body too large"));
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
                    ingestor.note_search_api("gdelt", Some(e.to_string())).await;
                    // Never retry below GDELT_MIN_RETRY_S — a sub-5s retry after a
                    // 429 is itself another rate-limit violation. (audit-news c)
                    backoff = (backoff * 2).clamp(Self::GDELT_MIN_RETRY_S, 500);
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
                        // Same hygiene as the RSS path: GDELT titles can carry
                        // entities, and its URLs the same tracking params.
                        let title = match art_d["title"].as_str() {
                            Some(t) if !t.is_empty() => sanitize_feed_text(t),
                            _ => continue,
                        };
                        if title.is_empty() { continue; }
                        let url_a = canonicalize_url(art_d["url"].as_str().unwrap_or(""));
                        if !ingestor.seen.lock().await.is_new(&url_a, &title) { continue; }
                        // Cross-feed title dedup: GDELT surfaces publisher headlines
                        // the RSS roster often already stored. (audit-news L1)
                        if !ingestor.titles.lock().await.is_new(&title) { continue; }

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
                    ingestor.note_search_api("gdelt", None).await;
                    if count > 0 {
                        info!("GDELT: {count} articles for '{query}'");
                    }
                }
            }

            sleep(Duration::from_secs(GDELT_QUERY_INTERVAL_S)).await;
        }
    }

    // ── Search-API loop health ────────────────────────────────────────────────

    /// Record a gnews/gdelt loop outcome where operators can SEE it: GET
    /// /api/sources serves last_success_at + consecutive_failures per API, so a
    /// dark loop (e.g. GDELT persistently 429-throttled) is visible on the wire
    /// instead of only inferable from a silently empty store. (audit-news c)
    async fn note_search_api(&self, api: &str, error: Option<String>) {
        let mut health = self.state.search_api_health.lock().await;
        let entry = health.entry(api.to_string()).or_default();
        let now = Utc::now().to_rfc3339();
        entry.last_attempt_at = Some(now.clone());
        match error {
            None => {
                entry.last_success_at      = Some(now);
                entry.consecutive_failures = 0;
                entry.last_error           = None;
            }
            Some(e) => {
                entry.consecutive_failures += 1;
                entry.last_error            = Some(e);
            }
        }
    }

    // ── Shared article store helper ───────────────────────────────────────────

    async fn store_article(&self, article: &RawArticle) {
        let body_excerpt: String = article.body.chars().take(500).collect(); // raised from 300
        // Update-in-place: a canonical URL already in the store is the SAME article
        // re-ingested after an edit (live-blog title churn produced 559 duplicate
        // URLs / 834 extra rows in an 11k store). Refresh title/body/published_at
        // on the EXISTING row — same id, so the JSONL append supersedes older lines
        // at reload — and don't recount it in the per-source registry. (audit-news L2)
        let updated = self.state.article_store.lock().await.update_by_url(
            &article.url,
            &article.source,
            &article.title,
            &body_excerpt,
            &article.published_at.to_rfc3339(),
            &article.fetched_at.to_rfc3339(),
        );
        if let Some(u) = updated {
            crate::aggregator::append_article(&u).await;
            return;
        }
        let stored = StoredArticle {
            id:           article.id.clone(),
            title:        article.title.clone(),
            url:          article.url.clone(),
            source:       article.source.clone(),
            tier:         article.source_tier as u8,
            published_at: article.published_at.to_rfc3339(),
            ingested_at:  article.fetched_at.to_rfc3339(),
            body:         body_excerpt,
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

    // ── URL canonicalization ──────────────────────────────────────────────────

    #[test]
    fn canonicalize_url_strips_tracking_params() {
        // Real decorations from the live store (audit-news L3): BBC at_medium/
        // at_campaign, SCMP utm_source. The clean article path must survive.
        assert_eq!(
            canonicalize_url("https://www.bbc.co.uk/news/articles/c24y2303ev8o?at_medium=RSS&at_campaign=rss"),
            "https://www.bbc.co.uk/news/articles/c24y2303ev8o");
        assert_eq!(
            canonicalize_url("https://www.scmp.com/news/world/article/3359383/europe?utm_source=rss_feed"),
            "https://www.scmp.com/news/world/article/3359383/europe");
    }

    #[test]
    fn canonicalize_url_strips_fragment_and_bare_question_mark() {
        assert_eq!(
            canonicalize_url("https://news.sky.com/story/x?postid=1#liveblog-body"),
            "https://news.sky.com/story/x?postid=1",
            "fragment goes, real query param stays");
        assert_eq!(canonicalize_url("https://www.scmp.com/article/123?"),
            "https://www.scmp.com/article/123", "bare trailing '?' dropped");
    }

    #[test]
    fn canonicalize_url_preserves_identifying_params() {
        // Non-tracking params can BE the article identity (CMS ids, pagination) —
        // stripping them would merge distinct articles. Order is preserved.
        assert_eq!(
            canonicalize_url("https://example.com/a?id=42&page=2&utm_medium=rss&CMP=x&smid=y&fbclid=z&gclid=w&ref=home"),
            "https://example.com/a?id=42&page=2");
        assert_eq!(canonicalize_url("https://example.com/plain"), "https://example.com/plain",
            "a URL with nothing to strip is unchanged");
    }

    #[test]
    fn seen_cache_tracking_param_variants_are_the_same_key() {
        // The dedup key must not treat per-fetch tracking churn as a new article.
        let mut cache = SeenCache::new(100);
        assert!(cache.is_new("https://bbc.co.uk/news/x?at_medium=RSS&at_campaign=rss", "Headline"));
        assert!(!cache.is_new("https://bbc.co.uk/news/x", "Headline"),
            "same canonical URL + title must be a duplicate");
    }

    // ── GNews outlet-suffix strip ─────────────────────────────────────────────

    #[test]
    fn gnews_outlet_suffix_is_stripped() {
        // Live examples: one Article-5 story stored ≥6× via "- Euronews" /
        // "- Euronews.com" / "- Reuters" variants alone (audit-news L4).
        assert_eq!(
            strip_gnews_outlet_suffix("NATO to reaffirm iron-clad commitment to Article 5 at Ankara summit - Euronews"),
            "NATO to reaffirm iron-clad commitment to Article 5 at Ankara summit");
        assert_eq!(
            strip_gnews_outlet_suffix("NATO to reaffirm iron-clad commitment to Article 5 at Ankara summit - Euronews.com"),
            "NATO to reaffirm iron-clad commitment to Article 5 at Ankara summit");
    }

    #[test]
    fn gnews_suffix_strip_spares_short_and_plain_titles() {
        assert_eq!(strip_gnews_outlet_suffix("US - China trade"), "US - China trade",
            "a short hyphenated clause must not be butchered");
        assert_eq!(strip_gnews_outlet_suffix("No suffix here"), "No suffix here");
    }

    // ── Feed-text sanitation ──────────────────────────────────────────────────

    #[test]
    fn sanitize_decodes_html_entities_named_and_numeric() {
        // Live titles: thecradle "&#039;", timesofindia "&amp;" (audit-news C1).
        assert_eq!(
            sanitize_feed_text("Yemen repels warplanes &#039;threatening&#039; airliner"),
            "Yemen repels warplanes 'threatening' airliner");
        assert_eq!(sanitize_feed_text("Hamas, Hezbollah &amp; Houthis"), "Hamas, Hezbollah & Houthis");
        assert_eq!(sanitize_feed_text("A &#x27;quoted&#x27; word"), "A 'quoted' word", "hex numeric form");
        assert_eq!(sanitize_feed_text("AT&T results & more"), "AT&T results & more",
            "a bare ampersand is not an entity and stays literal");
        assert_eq!(sanitize_feed_text("Q&Aé über die Lage — «détente» &amp; more"),
            "Q&Aé über die Lage — «détente» & more",
            "multibyte text right after '&' must not panic the entity window");
    }

    #[test]
    fn sanitize_strips_html_tags_from_bodies() {
        // middleeastmonitor bodies open with ~300 chars of pure markup, which then
        // IS the LLM's 600-byte excerpt (audit-news C3).
        assert_eq!(
            sanitize_feed_text("<div style=\"x\"><img src=\"y\"/></div><p>Troops advanced</p><p>at dawn.</p>"),
            "Troops advanced at dawn.");
        assert_eq!(sanitize_feed_text("truncated <a href=\"x"), "truncated",
            "an unterminated tag must not leak markup");
    }

    #[test]
    fn sanitize_cuts_feed_boilerplate_tails() {
        // WordPress + Guardian furniture (audit-news C4).
        assert_eq!(
            sanitize_feed_text("Strikes continued overnight. The post Strikes continued appeared first on The Times of Israel."),
            "Strikes continued overnight.");
        assert_eq!(
            sanitize_feed_text("Officials met in Cairo. Continue reading..."),
            "Officials met in Cairo.");
        assert_eq!(
            sanitize_feed_text("The post office reopened after the storm."),
            "The post office reopened after the storm.",
            "'The post …' without the WordPress footer phrase is real content");
    }

    // ── Junk filter ───────────────────────────────────────────────────────────

    #[test]
    fn junk_titles_and_sport_paths_are_rejected() {
        // Denylist titles (live: yonhap ×3, taipeitimes ×2) + real sports URLs
        // from the live store (audit-news L6 / off-topic flood).
        assert!(is_junk_entry("yonhap", "https://en.yna.co.kr/x", "Yonhap News Summary"));
        assert!(is_junk_entry("taipeitimes", "https://taipeitimes.com/x", "EDITORIAL CARTOON"));
        assert!(is_junk_entry("bbc",
            "https://www.bbc.co.uk/sport/tennis/articles/cddlqd0877mo", "Wimbledon day four"));
        assert!(is_junk_entry("guardian",
            "https://www.theguardian.com/sport/2026/jul/03/us-rugby-eagles-portugal", "Rugby preview"));
        assert!(is_junk_entry("guardian",
            "https://www.theguardian.com/football/live/2026/jul/03/world-cup-2026", "World Cup live"));
        assert!(is_junk_entry("abc_au",
            "https://www.abc.net.au/sport/2026-07-03/afl-round", "AFL round"));
    }

    #[test]
    fn junk_filter_never_touches_geopolitics() {
        // Path-based only: the same outlets' news sections — and sport-WORDED
        // geopolitics headlines — must always pass.
        assert!(!is_junk_entry("bbc",
            "https://www.bbc.co.uk/news/articles/c4gyv05gk4do", "Russia attacks Kyiv"));
        assert!(!is_junk_entry("guardian",
            "https://www.theguardian.com/world/2026/jul/03/ukraine-strikes", "Ukraine strikes"));
        assert!(!is_junk_entry("guardian",
            "https://www.theguardian.com/world/x", "Football diplomacy: World Cup politics"));
        assert!(!is_junk_entry("aljazeera",
            "https://aljazeera.com/sport/x", "Sport section of a non-listed source passes"));
    }

    // ── Cross-feed title dedup ────────────────────────────────────────────────

    #[test]
    fn title_dedup_drops_exact_cross_feed_duplicates() {
        // Live: one NATO headline stored verbatim by 3 different feeds (audit-news L1).
        let mut td = TitleDedup::new(100);
        assert!(td.is_new("NATO leaders to gather in Ankara, aiming to smooth over tensions with Trump"));
        assert!(!td.is_new("NATO leaders to gather in Ankara, aiming to smooth over tensions with Trump"),
            "identical headline from a second feed must be dropped");
    }

    #[test]
    fn title_dedup_normalizes_case_and_punctuation() {
        // Live near-dup class: "…Iran's supreme leader" vs "…Iran's Supreme Leader".
        let mut td = TitleDedup::new(100);
        assert!(td.is_new("Strike ordered on Iran's supreme leader"));
        assert!(!td.is_new("Strike ordered on Iran’s Supreme Leader!"),
            "case/punctuation variants of one headline must match");
    }

    #[test]
    fn title_dedup_short_generic_titles_bypass() {
        // "Watch live" from two outlets is two DIFFERENT streams — too generic to
        // dedup safely, so short titles always pass.
        let mut td = TitleDedup::new(100);
        assert!(td.is_new("Watch live"));
        assert!(td.is_new("Watch live"));
    }

    #[test]
    fn title_dedup_evicts_at_max_size() {
        let mut td = TitleDedup::new(2);
        assert!(td.is_new("first long headline about events"));
        assert!(td.is_new("second long headline about events"));
        assert!(td.is_new("third long headline about events")); // first evicted
        assert!(td.is_new("first long headline about events"),
            "FIFO eviction must bound memory like SeenCache");
    }

    #[test]
    fn title_dedup_expires_so_recurring_headlines_are_new_editions() {
        // A verbatim-recurring wire headline days later ("N. Korea fires ballistic
        // missile toward East Sea", a weekly franchise title) is a NEW incident or
        // edition — only same-news-cycle repeats are duplicates. Keys age out at the
        // TTL instead of blocking for the life of the 50k FIFO (~1-2 weeks).
        let mut td = TitleDedup::new(100);
        let t0 = 1_780_000_000;
        assert!(td.is_new_at("north korea fires ballistic missile toward east sea", t0));
        assert!(!td.is_new_at("north korea fires ballistic missile toward east sea", t0 + 3600),
            "same news cycle → duplicate");
        assert!(td.is_new_at("north korea fires ballistic missile toward east sea",
            t0 + TITLE_DEDUP_TTL_S + 1),
            "past the TTL the same headline is a new edition, not a copy");
        assert!(!td.is_new_at("north korea fires ballistic missile toward east sea",
            t0 + TITLE_DEDUP_TTL_S + 3600),
            "the refreshed key blocks again within the new window");
    }

    // ── entry_to_article (through a real feed parse) ──────────────────────────

    fn entries_from_rss(items: &str) -> Vec<feed_rs::model::Entry> {
        let xml = format!(
            "<?xml version=\"1.0\"?><rss version=\"2.0\"><channel><title>t</title>{items}</channel></rss>");
        parse_feed_raw(xml.as_bytes()).expect("test feed parses").entries
    }

    #[test]
    fn entry_to_article_rejects_markup_titles_and_cleans_fields() {
        // First item: the real skynews live-blog junk shape — the "title" is
        // literally an anchor tag (audit-news C2). Second: entities + tracking URL.
        let entries = entries_from_rss(
            "<item><title>&lt;a href='https://news.sky.com/story/x?postid=1'&gt;Ukrainian man charged</title>\
              <link>https://news.sky.com/story/x</link></item>\
             <item><title>Hamas, Hezbollah &amp;amp; Houthis: what next</title>\
              <link>https://timesofindia.com/world/a1.cms?utm_source=rss&amp;utm_medium=feed</link>\
              <description>&lt;p&gt;Analysts said&lt;/p&gt; the axis is shifting</description></item>");
        assert_eq!(entries.len(), 2);
        // feed-rs 2.x hands the markup title through as "a href='…'Ukrainian man
        // charged" (brackets stripped, tag guts inline) — still junk, still rejected.
        assert!(entry_to_article(&entries[0], "skynews", SourceTier::Tier1).is_none(),
            "a title that is (mangled) markup must be rejected");
        let a = entry_to_article(&entries[1], "timesofindia", SourceTier::Tier2)
            .expect("clean entry ingests");
        assert_eq!(a.title, "Hamas, Hezbollah & Houthis: what next", "entities decoded in title");
        assert_eq!(a.url, "https://timesofindia.com/world/a1.cms", "tracking params stripped from stored URL");
        assert_eq!(a.body, "Analysts said the axis is shifting", "tags stripped from body");
    }

    #[test]
    fn entry_to_article_strips_gnews_suffix_only_for_gnews() {
        let entries = entries_from_rss(
            "<item><title>North Korea conducts third missile launch - Reuters</title>\
              <link>https://news.google.com/rss/articles/CBMi123</link></item>");
        let g = entry_to_article(&entries[0], "gnews", SourceTier::Tier2).unwrap();
        assert_eq!(g.title, "North Korea conducts third missile launch",
            "gnews outlet suffix stripped before dedup keys and storage");
        let r = entry_to_article(&entries[0], "nknews", SourceTier::Tier2).unwrap();
        assert_eq!(r.title, "North Korea conducts third missile launch - Reuters",
            "publisher feeds keep their titles verbatim");
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
        let feed  = parse_feed_raw(bytes.as_ref()).map_err(|e| format!("parse: {e}"))?;
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
