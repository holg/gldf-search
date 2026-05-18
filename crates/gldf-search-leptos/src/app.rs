//! `<App/>` root and the single-page layout.
//!
//! State model:
//!
//! - One [`RwSignal<Filter>`] owned by `App`. Every interaction
//!   (search box typing, facet toggle, reset) writes here.
//! - Two `Resource`s derive their input from the filter signal: one
//!   for the hit list (`SearchDocs`), one for the facet panel
//!   (`FetchFacets`). When the filter changes, Leptos invalidates the
//!   resources and they re-fetch through the server fns.
//! - Components are pure renderers; they take the filter signal or a
//!   slice of resource output and produce view.

use gldf_search_index::{FacetField, Pagination, SearchHit};
use gldf_search_schema::filter::Filter;
use leptos::prelude::*;
use leptos_meta::{provide_meta_context, HashedStylesheet, MetaTags, Title};
use leptos_router::components::{Route, Router, Routes};
use leptos_router::StaticSegment;

use crate::api::{fetch_facets, fetch_range_bounds, lookup_article, search_docs};
use crate::components::{
    ArticleBar, FacetPanel, FilterChips, FulltextToggle, HitRow, NumericRangeInputs, SearchBar,
    VariantRow,
};

/// Default-fetched facet fields. All 11 variants from `FacetField` so
/// the panel can render every group. The render layer ignores empty
/// groups (corpora that don't carry e.g. Applications still cost
/// nothing at SSR time — the index returns an empty `Vec`).
const FACET_FIELDS: &[FacetField] = &[
    FacetField::Applications,
    FacetField::LightDistribution,
    FacetField::MountingPlace,
    FacetField::MountingType,
    FacetField::ProductForm,
    FacetField::IpCode,
    FacetField::SafetyClass,
    FacetField::IkRating,
    FacetField::Adjustability,
    FacetField::Labels,
    FacetField::Manufacturer,
    FacetField::LampType,
    FacetField::ControlGearInterface,
    FacetField::EmergencyLightingType,
    FacetField::HasPhotometry,
    FacetField::Has3d,
];

/// HTML shell. The server side renders this once per request; the
/// browser hydrates the body. `options` are surfaced through Leptos's
/// own context — the server installs them, this function reads them.
///
/// The stylesheet `<link>` is emitted *here* (in `<head>`, SSR-side
/// only) via [`HashedStylesheet`]. `HashedStylesheet` reads
/// `target/site/hash.txt` (or whatever `LeptosOptions::hash_file`
/// points at — actually the path is `current_exe().parent().join(
/// hash_file)`) and constructs the correct href including the content
/// hash, e.g. `/pkg/gldf-search.<hash>.css`. The browser doesn't run
/// this code; it just sees the resolved `<link>` in the static HTML.
/// This keeps the SSR/CSR markup symmetric and unlocks aggressive
/// nginx caching by content hash.
///
/// Called by `leptos_axum::generate_route_list(shell)` and used as the
/// shell parameter to `LeptosRoutes::leptos_routes(shell)`.
pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                <AutoReload options=options.clone()/>
                <HydrationScripts options=options.clone()/>
                <HashedStylesheet options=options id="leptos"/>
                <MetaTags/>
            </head>
            <body>
                <App/>
            </body>
        </html>
    }
}

/// Top-of-tree component. Wires meta tags, the router, and the search
/// page. Renders only the **body** content; `<html>`/`<head>` come
/// from [`shell`] — including the stylesheet link.
#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();

    view! {
        <Title text="gldf-search"/>
        <Router>
            <main>
                <Routes fallback=|| "Not found.">
                    <Route path=StaticSegment("") view=SearchPage/>
                </Routes>
            </main>
        </Router>
    }
}

/// `/` — the only page (for now).
#[component]
fn SearchPage() -> impl IntoView {
    // Hydrate the filter signal from `window.location.search` on the
    // client side so deep links like
    // `/?text=led&app=10,12&flux=1000-5000` boot straight into a
    // populated search. SSR can't read the browser URL meaningfully
    // (the server has the full request URL but the URL bar is the
    // client's source of truth), so on the SSR pass we just use
    // `Filter::default()`. The hydrate-side Effect installed below
    // immediately overwrites the URL after the filter signal is
    // seeded, so SSR ↔ hydrate divergence resolves on the first
    // render tick.
    #[cfg(target_arch = "wasm32")]
    let initial = crate::url_sync::read_filter_from_url();
    #[cfg(not(target_arch = "wasm32"))]
    let initial = Filter::default();
    let filter = RwSignal::new(initial);

    // Mirror filter → URL via `history.replaceState`. Effect tracks
    // `filter`, so every change (slider drag, chip toggle, text
    // input) rewrites the address bar without polluting the back
    // stack. No-op on SSR.
    #[cfg(target_arch = "wasm32")]
    crate::url_sync::install_url_writer(filter);

    // Article-number override. `None` ⇒ normal filter-driven search;
    // `Some(code)` ⇒ results section ignores the filter and shows
    // only docs whose `gtin` or `product_code` matches `code`. The
    // sidebar (facets + ranges) stays driven by the filter signal so
    // the user can compare the article against the active filter
    // context without losing it.
    let article = RwSignal::new(Option::<String>::None);

    // Full-text gate. The free-text scan over 270k docs is expensive
    // on the single-core production server (multi-second). To stop
    // users from triggering it by accident, Enter on the search bar
    // only commits to `filter.text` when this signal is true. The
    // user has to tick a checkbox; the toggle then auto-disarms 10
    // seconds after the last keystroke / submission.
    //
    // We CANNOT cancel an in-flight server-side scan — the linear
    // scan is a synchronous Rust loop inside one tokio task. So the
    // semantic on auto-disarm is: any pending query finishes and its
    // result is displayed, but the next Enter is ignored until the
    // user re-arms the toggle.
    let fulltext_armed = RwSignal::new(false);
    let disarm_seconds = RwSignal::new(0u32);

    // Per-subdomain manufacturer lock. Returns `Some(name)` when the
    // request came in on a `<mfr>.gldf-search.de` subdomain (nginx
    // sets `X-Manufacturer`), `None` on the apex domain.
    //
    // The server already pre-applies the scope to every server-fn
    // filter; this Resource exists purely so the UI knows whether to
    // hide the Manufacturer facet and render a "scoped to <name>"
    // banner. Read-once on page load.
    let manufacturer_scope = Resource::new(|| (), |_| async move {
        crate::api::fetch_manufacturer_scope().await
    });

    // Article-lookup resource — fires only when `article` is Some.
    let article_hits = Resource::new(
        move || article.get(),
        |art| async move {
            match art {
                Some(code) => lookup_article(code).await,
                None => Ok(Vec::new()),
            }
        },
    );

    // Facet counts resource — same dependency, different server fn.
    let facets = Resource::new(
        move || filter.get(),
        |f| async move { fetch_facets(Some(f), Some(FACET_FIELDS.to_vec())).await },
    );

    // Per-axis min/max bounds for the six numeric range sliders. Same
    // filter signal; the server fn walks the corpus 6 times (once per
    // axis, with that axis relaxed) and returns the dynamic rails.
    let bounds = Resource::new(
        move || filter.get(),
        |f| async move { fetch_range_bounds(Some(f)).await },
    );

    // Mirror the scope Resource into a plain signal so non-Suspense
    // child reads work — same pattern as `current_bounds` in
    // NumericRangeInputs (Leptos 0.8 Resources only resolve from
    // Suspense / Transition / Effect scopes).
    let scope_name: RwSignal<Option<String>> = RwSignal::new(None);
    Effect::new(move |_| {
        if let Some(Ok(Some(name))) = manufacturer_scope.get() {
            scope_name.set(Some(name));
        }
    });
    let manufacturer_locked = Memo::new(move |_| scope_name.with(|s| s.is_some()));

    view! {
        <header class="site-header">
            <h1>
                "gldf-search"
                {move || scope_name.get().map(|n| view! {
                    " · "<span class="scope-name">{n}</span>
                })}
            </h1>
            <p class="tagline">
                {move || match scope_name.get() {
                    Some(n) => format!("Showing {n} luminaires only"),
                    None => "Search the photometric corpus".to_string(),
                }}
            </p>
        </header>
        <div class="top-bar">
            <ArticleBar article/>
            <SearchBar filter fulltext_armed disarm_seconds/>
        </div>
        <FulltextToggle fulltext_armed disarm_seconds/>
        <div class="layout">
            <div class="sidebar">
                <Suspense fallback=|| view! { <aside class="facets"><p>"Loading facets…"</p></aside> }>
                    {move || {
                        let locked = manufacturer_locked.get();
                        facets.get().map(|res| match res {
                            Ok(counts) => view! {
                                <FacetPanel filter counts manufacturer_locked=locked/>
                            }.into_any(),
                            Err(e) => view! {
                                <aside class="facets">
                                    <p class="error">"Facet error: " {e.to_string()}</p>
                                </aside>
                            }.into_any(),
                        })
                    }}
                </Suspense>
                <NumericRangeInputs filter bounds/>
            </div>
            <section class="results">
                <FilterChips filter manufacturer_locked=manufacturer_locked.get()/>
                {move || {
                    // Branch: article lookup takes precedence when set.
                    // Both branches return AnyView; React/Leptos picks
                    // the one whose signal currently has a value.
                    if article.with(|a| a.is_some()) {
                        view! {
                            <ArticleResults article article_hits/>
                        }.into_any()
                    } else {
                        view! {
                            <FilterResults filter/>
                        }.into_any()
                    }
                }}
            </section>
        </div>
    }
}

/// Render the filter-driven hit list (the default mode).
/// Page size for the infinite-scroll hit list. 50 is the same
/// `Pagination::DEFAULT::limit` the server uses; staying aligned
/// avoids server-side recompute on partial pages.
const PAGE_SIZE: u32 = 50;

#[component]
fn FilterResults(filter: RwSignal<Filter>) -> impl IntoView {
    // Running concatenation of every page loaded so far for the
    // current filter. Reset to empty whenever the filter signal
    // changes.
    let loaded = RwSignal::new(Vec::<SearchHit>::new());
    // Total hits across all pages, returned by the server on
    // every search call. `None` until the first page lands.
    let total = RwSignal::new(Option::<u32>::None);
    // Offset for the next page. Bumped by `PAGE_SIZE` on every
    // successful append.
    let next_offset = RwSignal::new(0u32);
    // True while a page request is in flight — prevents the
    // IntersectionObserver from firing duplicate fetches when the
    // user scrolls fast.
    let loading = RwSignal::new(false);
    // Set when the server returns a non-OK status; surfaced inline
    // so the user can retry.
    let error = RwSignal::new(Option::<String>::None);
    // Generation counter — every filter change bumps it. Spawned
    // fetch tasks check `gen` before committing their result so
    // late-arriving responses from a stale filter never poison the
    // running list.
    let gen = RwSignal::new(0u64);

    // Drives the "load the next page" call. Captured as `Arc<Fn>` so
    // it can be shared between the filter-change effect, the
    // IntersectionObserver callback, and the "Load more" button.
    //
    // EVERY signal read inside this closure must be `_untracked` —
    // otherwise the filter-change Effect (which calls this closure)
    // would pick up dependencies on `total`, `loaded`, `loading`,
    // `gen`, `next_offset`, `filter` — and re-fire on every signal
    // write the closure itself produces (the spawn_local's success
    // path sets `total` and `loaded` — boom, infinite loop). Reading
    // untracked keeps the Effect's only tracking dep the `filter`
    // read in its own body.
    let load_next_page = std::sync::Arc::new(move || {
        if loading.get_untracked() {
            return;
        }
        if let Some(t) = total.get_untracked() {
            if loaded.with_untracked(|l| l.len() as u32) >= t {
                return;
            }
        }
        loading.set(true);
        error.set(None);
        let my_gen = gen.get_untracked();
        let offset = next_offset.get_untracked();
        let f = filter.get_untracked();
        leptos::task::spawn_local(async move {
            let resp = search_docs(
                Some(f),
                Some(Pagination {
                    offset,
                    limit: PAGE_SIZE,
                }),
            )
            .await;
            // CRITICAL: clear `loading` BEFORE the gen-stale check.
            // Otherwise, when the user changes the filter mid-flight,
            // the new spawn would set loading=true, this branch
            // returns without clearing it, and EVERY subsequent
            // `load_next_page` invocation early-exits at the
            // `if loading.get_untracked() { return; }` guard at the
            // top — search never fires again. The result was the
            // "facets work, results stuck on Searching" hang the
            // user reported.
            loading.set(false);
            // Drop result if the user typed something else while we
            // were fetching. `_untracked` here too — the spawn_local
            // body runs outside any Effect tracking context, but the
            // pattern stays consistent.
            if gen.get_untracked() != my_gen {
                return;
            }
            match resp {
                Ok(result) => {
                    total.set(Some(result.total));
                    let n = result.hits.len() as u32;
                    loaded.update(|v| v.extend(result.hits));
                    next_offset.update(|o| *o += n);
                }
                Err(e) => {
                    error.set(Some(e.to_string()));
                }
            }
        });
    });

    // Reset + fire page 0 whenever the filter changes. The effect
    // runs on the hydrate side only — SSR draws an empty result list
    // until the first page lands.
    {
        let load = load_next_page.clone();
        Effect::new(move |_| {
            // Tracking dep: read the filter so changes re-fire.
            let _ = filter.get();
            // Bump generation, reset state, kick off page 0. None of
            // these writes are reactive deps because we never read
            // them above; `load()` reads via *_untracked so it does
            // not register them either.
            gen.update(|g| *g += 1);
            loaded.set(Vec::new());
            total.set(None);
            next_offset.set(0);
            error.set(None);
            // Force-clear `loading` so a filter change mid-flight
            // doesn't get blocked by the prior request's lingering
            // flag. The in-flight task's response is discarded by
            // the gen check; whether it eventually clears the flag
            // or not, we don't want to stall the new spawn. Two
            // concurrent search_docs are fine — the latest gen wins.
            loading.set(false);
            load();
        });
    }

    let load_for_btn = load_next_page.clone();

    // Sentinel ref — kept for future infinite-scroll re-attach but
    // currently unused. The previous IntersectionObserver impl
    // continually fired "intersecting" while the sentinel stayed in
    // the viewport (which it does for the first few pages until the
    // list is tall enough to push it off-screen), causing each
    // successful response to immediately trigger another load —
    // load 50 → response → loading=false → still intersecting →
    // load again, ad infinitum. After 200+ unintended pages the
    // user saw "10150 of 270400 matches" instead of "50 of …".
    //
    // The Load-more button below is the v1 affordance. Re-add the
    // observer once we have a real "user has scrolled past this
    // point" check (rootMargin negative + disconnect-after-fire
    // semantics).
    let sentinel: NodeRef<leptos::html::Div> = NodeRef::new();

    view! {
        // Before the first page has landed we only paint the
        // "Searching…" line plus any error. Hiding DocList AND the
        // Load-more button during the first roundtrip avoids the
        // confusing "Searching… / No matches. / Loading…" stack the
        // user sees when those three states are read live.
        {move || {
            if total.get().is_none() {
                return view! {
                    <p class="result-count">"Searching…"</p>
                    {error.get().map(|e| view! {
                        <p class="error">"Search error: " {e}</p>
                    })}
                }.into_any();
            }
            // First page (or later) has landed. Drive the steady-
            // state UI: count line, error notice, hit list, infinite-
            // scroll sentinel + Load-more fallback.
            let load = load_for_btn.clone();
            view! {
                <p class="result-count">
                    {move || {
                        let n = loaded.with(|v| v.len());
                        let t = total.get().unwrap_or(n as u32);
                        format!("{n} of {t} matches")
                    }}
                </p>
                {error.get().map(|e| view! {
                    <p class="error">"Search error: " {e}</p>
                })}
                <VariantList
                    hits=Signal::derive(move || loaded.get())
                    filter
                />
                {move || {
                    let done = total.get().is_some_and(|t| loaded.with(|v| v.len() as u32) >= t);
                    if done {
                        ().into_any()
                    } else {
                        let load = load.clone();
                        view! {
                            <div class="hit-sentinel" node_ref=sentinel></div>
                            <button
                                class="load-more"
                                disabled=move || loading.get()
                                on:click=move |_| load()
                            >
                                {move || if loading.get() { "Loading…" } else { "Load more" }}
                            </button>
                        }.into_any()
                    }
                }}
            }.into_any()
        }}
    }
}

/// Render the article-number lookup results. Always shows a small
/// "Showing article: <code>" notice + a × that clears `article`,
/// returning the page to filter-driven search.
#[component]
fn ArticleResults(
    article: RwSignal<Option<String>>,
    article_hits: Resource<Result<Vec<crate::api::DocPayload>, ServerFnError>>,
) -> impl IntoView {
    view! {
        <p class="article-notice">
            {move || {
                article.with(|a| {
                    a.as_deref()
                        .map(|s| format!("Article #: {s}"))
                        .unwrap_or_default()
                })
            }}
            <button
                class="article-clear"
                on:click=move |_| article.set(None)
                title="Back to filter search"
            >
                "×"
            </button>
        </p>
        <Suspense fallback=|| view! { <p>"Looking up article…"</p> }>
            {move || {
                article_hits.get().map(|res| match res {
                    Ok(list) if list.is_empty() => view! {
                        <p class="empty">"No matching article."</p>
                    }.into_any(),
                    Ok(list) => view! {
                        <p class="result-count">{list.len()} " match" {if list.len() == 1 { "" } else { "es" }}</p>
                        <ul class="hit-list">
                            {list.into_iter().map(|payload| view! {
                                <HitRow doc=payload.doc viewer_url=payload.viewer_url/>
                            }).collect_view()}
                        </ul>
                    }.into_any(),
                    Err(e) => view! {
                        <p class="error">"Article lookup error: " {e.to_string()}</p>
                    }.into_any(),
                })
            }}
        </Suspense>
    }
}

/// Render a list of variant-grain search hits. Each hit resolves to
/// one `<VariantRow>` showing the family header (manufacturer /
/// product) plus that specific variant's photometry and source.
///
/// The Resource re-fires whenever `hits` changes — infinite scroll
/// appends and the new tail is fetched in one round-trip.
/// `fetch_variants` preserves input order so appended rows always
/// show at the bottom.
#[component]
fn VariantList(
    /// Running list of variant-grain hits (grows on infinite-scroll).
    #[prop(into)] hits: Signal<Vec<SearchHit>>,
    filter: RwSignal<Filter>,
) -> impl IntoView {
    let _ = filter; // reserved for future row-level interactions
    let payloads = Resource::new(
        move || hits.get(),
        |hits| async move {
            if hits.is_empty() {
                return Vec::new();
            }
            let refs: Vec<crate::api::VariantRef> = hits
                .into_iter()
                .map(|h| crate::api::VariantRef {
                    doc_id: h.doc.0.iter().map(|b| format!("{b:02x}")).collect(),
                    variant: h.variant,
                })
                .collect();
            crate::api::fetch_variants(refs).await.unwrap_or_default()
        },
    );

    view! {
        <Suspense fallback=|| view! { <p>"Fetching docs…"</p> }>
            {move || {
                payloads.get().map(|list| {
                    if list.is_empty() {
                        view! { <p class="empty">"No matches."</p> }.into_any()
                    } else {
                        view! {
                            <ul class="hit-list">
                                {list.into_iter().map(|payload| view! {
                                    <VariantRow payload/>
                                }).collect_view()}
                            </ul>
                        }.into_any()
                    }
                })
            }}
        </Suspense>
    }
    .into_any()
}
