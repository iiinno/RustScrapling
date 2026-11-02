//! The `rustscrapling` command line tool (Plan section 7.1).
//! Stage 2 adds the local selection commands: select, similar, relocate.
//! Fetching commands arrive with the fetch crates in later stages.

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use rustscrapling_adaptive::{find_similar, relocate, SqliteStorage};
use rustscrapling_dom::html5_backend::{parse_html as parse_html5, Html5Options};
use rustscrapling_dom::libxml_backend::{parse_html as parse_libxml, CompatOptions};
use rustscrapling_dom::{Dom, NodeId};
use rustscrapling_select::css::css;
use rustscrapling_select::text_search::{
    find_by_regex, find_by_text, FindByRegexOpts, FindByTextOpts,
};
use rustscrapling_select::xpath::xpath;
use rustscrapling_select::Selection;

#[derive(Parser)]
#[command(name = "rustscrapling", version, about = "RustScrapling: adaptive web scraping, Scrapling-compatible")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Select elements in a local HTML file (URLs are fetched from stage 3 on).
    Select {
        /// HTML file path, or "-" for stdin.
        target: String,
        /// CSS selector.
        #[arg(long, conflicts_with_all = ["xpath_expr", "text", "regex"])]
        css: Option<String>,
        /// XPath expression.
        #[arg(long = "xpath", conflicts_with_all = ["text", "regex"])]
        xpath_expr: Option<String>,
        /// Find elements by their own text.
        #[arg(long, conflicts_with = "regex")]
        text: Option<String>,
        /// Find elements whose own text matches a regex.
        #[arg(long)]
        regex: Option<String>,
        /// Partial text match (with --text).
        #[arg(long)]
        partial: bool,
        /// Return all matches instead of the first (with --text/--regex).
        #[arg(long)]
        all: bool,
        /// Case-sensitive text/regex match.
        #[arg(long)]
        case_sensitive: bool,
        /// Do not clean whitespace before text/regex matching.
        #[arg(long)]
        no_clean: bool,
        /// Parser: "compat" (libxml2, Scrapling-identical) or "html5".
        #[arg(long, default_value = "compat")]
        parser: String,
        /// Adaptive identifier (with --auto-save / --adaptive).
        #[arg(long)]
        id: Option<String>,
        /// Save matched elements under --id for later relocation.
        #[arg(long)]
        auto_save: bool,
        /// If nothing matches, relocate the element saved under --id.
        #[arg(long)]
        adaptive: bool,
        /// Minimum relocation score (percent).
        #[arg(long, default_value_t = 40)]
        percentage: i64,
        /// Adaptive storage file.
        #[arg(long, default_value = "rustscrapling_storage.db")]
        storage_file: String,
        /// Page URL (separates adaptive storage by domain).
        #[arg(long)]
        url: Option<String>,
    },
    /// Find elements similar to the one matched by --css/--xpath.
    Similar {
        /// HTML file path, or "-" for stdin.
        target: String,
        /// CSS selector picking the reference element.
        #[arg(long, conflicts_with = "xpath_expr")]
        css: Option<String>,
        /// XPath expression picking the reference element.
        #[arg(long = "xpath")]
        xpath_expr: Option<String>,
        /// Similarity threshold 0..1.
        #[arg(long, default_value_t = 0.2)]
        threshold: f64,
        /// Attributes to ignore when comparing (repeatable).
        #[arg(long = "ignore", default_values_t = ["href".to_string(), "src".to_string()])]
        ignore: Vec<String>,
        /// Also match on element text.
        #[arg(long)]
        match_text: bool,
        /// Parser: "compat" or "html5".
        #[arg(long, default_value = "compat")]
        parser: String,
    },
    /// Relocate an element previously saved under --id.
    Relocate {
        /// HTML file path, or "-" for stdin.
        target: String,
        /// Adaptive identifier.
        #[arg(long)]
        id: String,
        /// Minimum relocation score (percent).
        #[arg(long, default_value_t = 40)]
        percentage: i64,
        /// Adaptive storage file.
        #[arg(long, default_value = "rustscrapling_storage.db")]
        storage_file: String,
        /// Page URL (separates adaptive storage by domain).
        #[arg(long)]
        url: Option<String>,
        /// Parser: "compat" or "html5".
        #[arg(long, default_value = "compat")]
        parser: String,
    },
}

fn load_dom(target: &str, parser: &str) -> Result<Dom, String> {
    let content = if target == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("reading stdin: {e}"))?;
        buf
    } else if target.starts_with("http://") || target.starts_with("https://") {
        return Err("URL targets arrive with the fetch commands in stage 3; pass a local file for now".into());
    } else {
        std::fs::read_to_string(target).map_err(|e| format!("reading {target}: {e}"))?
    };
    match parser {
        "compat" => parse_libxml(&content, &CompatOptions::default()),
        "html5" => parse_html5(&content, &Html5Options::default()),
        other => Err(format!("unknown parser '{other}' (compat|html5)")),
    }
}

fn print_paths(dom: &Dom, ids: &[NodeId]) {
    for &id in ids {
        println!("{}", dom.element_path(id));
    }
}

fn element_ids(selections: Vec<Selection>) -> Vec<NodeId> {
    selections
        .into_iter()
        .filter_map(|s| match s {
            Selection::Element(id) => Some(id),
            Selection::Text(_) => None,
        })
        .collect()
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    match cli.command {
        Command::Select {
            target,
            css: css_expr,
            xpath_expr,
            text,
            regex,
            partial,
            all,
            case_sensitive,
            no_clean,
            parser,
            id,
            auto_save,
            adaptive,
            percentage,
            storage_file,
            url,
        } => {
            let dom = load_dom(&target, &parser)?;
            let root = dom.document_element().ok_or("document has no root element")?;
            let mut results: Vec<NodeId> = Vec::new();
            let mut texts: Vec<String> = Vec::new();
            if let Some(expr) = &css_expr {
                for sel in css(&dom, root, expr).map_err(|e| e.to_string())? {
                    match sel {
                        Selection::Element(id) => results.push(id),
                        Selection::Text(t) => texts.push(t),
                    }
                }
            } else if let Some(expr) = &xpath_expr {
                for sel in xpath(&dom, root, expr, &[]).map_err(|e| e.to_string())? {
                    match sel {
                        Selection::Element(id) => results.push(id),
                        Selection::Text(t) => texts.push(t),
                    }
                }
            } else if let Some(text) = &text {
                results = find_by_text(
                    &dom,
                    root,
                    text,
                    FindByTextOpts {
                        first_match: !all,
                        partial,
                        case_sensitive,
                        clean_match: !no_clean,
                    },
                );
            } else if let Some(pattern) = &regex {
                results = find_by_regex(
                    &dom,
                    root,
                    pattern,
                    FindByRegexOpts {
                        first_match: !all,
                        case_sensitive,
                        clean_match: !no_clean,
                    },
                )
                .map_err(|e| format!("invalid regex: {e}"))?;
            } else {
                return Err("one of --css, --xpath, --text, --regex is required".into());
            }

            let storage = if id.is_some() && (auto_save || adaptive) {
                Some(
                    SqliteStorage::open(&storage_file, url.as_deref())
                        .map_err(|e| format!("opening storage: {e}"))?,
                )
            } else {
                None
            };
            if let (Some(storage), Some(id)) = (&storage, &id) {
                if adaptive && results.is_empty() {
                    if let Some(dict) = storage
                        .retrieve(id)
                        .map_err(|e| format!("retrieving from storage: {e}"))?
                    {
                        results = relocate(&dom, root, &dict, percentage);
                    }
                }
                if auto_save {
                    for &el in &results {
                        storage
                            .save_element(&dom, el, id)
                            .map_err(|e| format!("saving to storage: {e}"))?;
                    }
                }
            }
            print_paths(&dom, &results);
            for t in &texts {
                println!("{t}");
            }
            Ok(())
        }
        Command::Similar {
            target,
            css: css_expr,
            xpath_expr,
            threshold,
            ignore,
            match_text,
            parser,
        } => {
            let dom = load_dom(&target, &parser)?;
            let root = dom.document_element().ok_or("document has no root element")?;
            let base = if let Some(expr) = &css_expr {
                element_ids(css(&dom, root, expr).map_err(|e| e.to_string())?)
                    .into_iter()
                    .next()
            } else if let Some(expr) = &xpath_expr {
                element_ids(xpath(&dom, root, expr, &[]).map_err(|e| e.to_string())?)
                    .into_iter()
                    .next()
            } else {
                return Err("one of --css or --xpath is required".into());
            }
            .ok_or("reference element not found")?;
            let ignore_refs: Vec<&str> = ignore.iter().map(|s| s.as_str()).collect();
            let found = find_similar(&dom, base, threshold, &ignore_refs, match_text);
            print_paths(&dom, &found);
            Ok(())
        }
        Command::Relocate {
            target,
            id,
            percentage,
            storage_file,
            url,
            parser,
        } => {
            let dom = load_dom(&target, &parser)?;
            let root = dom.document_element().ok_or("document has no root element")?;
            let storage = SqliteStorage::open(&storage_file, url.as_deref())
                .map_err(|e| format!("opening storage: {e}"))?;
            let dict = storage
                .retrieve(&id)
                .map_err(|e| format!("retrieving from storage: {e}"))?
                .ok_or_else(|| format!("no element saved under id '{id}'"))?;
            let found = relocate(&dom, root, &dict, percentage);
            print_paths(&dom, &found);
            Ok(())
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
