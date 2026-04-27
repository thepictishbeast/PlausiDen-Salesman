//! salesman-discovery — bring companies into the system.
//!
//! Two adapters land in Phase 1.1:
//! - **CsvSeed**: read a CSV the operator hands us. Required columns
//!   `display_name`, `homepage`. Optional: `industry`, `region`,
//!   `description`, `legal_name`.
//! - **HomepageFetcher**: HTTP GET a company's homepage and pull
//!   title + meta description + tech-detection fingerprints.
//!
//! Each adapter also implements `Tool` so the agent loop can drive
//! them.
//!
//! BUG ASSUMPTION: HomepageFetcher only fetches the document at the
//! given URL — no follow links, no JS execution. For JS-rendered sites
//! we'll bring in PlausiDen-Crawler RPC in a later phase.
//!
//! SECURITY: requests time out at 15s and are size-capped at 4MiB.
//! We don't follow more than 5 redirects.
#![forbid(unsafe_code)]

pub mod brave_search;
pub mod csv_seed;
pub mod email_pattern;
pub mod homepage;
pub mod team_scraper;
pub mod tools;

pub use brave_search::{BraveSearch, BraveSearchTool, SearchHit};
pub use csv_seed::CsvSeed;
pub use email_pattern::{EmailPatternGuesser, EmailPatternTool, GuessedEmail};
pub use homepage::HomepageFetcher;
pub use team_scraper::{BuyerCandidate, TeamScraper};
pub use tools::{CsvSeedTool, HomepageFetchTool};
