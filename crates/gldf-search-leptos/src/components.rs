//! Reusable view fragments: search bar, hit row, facet panel.
//!
//! Components are intentionally dumb — no state of their own. The
//! parent owns the [`Filter`] signal and passes derived bits down.
//! That keeps the reactivity story linear: one filter signal, one
//! resource for results, one for facets.

use compact_str::CompactString;
use gldf_search_index::{
    ArticleSuggestion, F32Bounds, FacetCounts, FacetSuggestion, RangeBounds, U16Bounds, U32Bounds,
};
use gldf_search_schema::doc::{LuminaireDoc, MountingPlace, MountingType};
use gldf_search_schema::enums::{
    adjustability_from_str, adjustability_str, application_from_str, application_mid,
    application_str, application_top, control_gear_interface_from_str, control_gear_interface_str,
    emergency_lighting_type_from_str, emergency_lighting_type_str, ik_rating_from_str,
    ik_rating_str, ip_code_from_str, ip_code_str, label_from_str, label_str, lamp_type_from_str,
    lamp_type_str, light_distribution_from_str, light_distribution_str, product_form_from_str,
    product_form_str, safety_class_from_str, safety_class_str,
};
use gldf_search_schema::filter::Filter;
use leptos::prelude::*;
use leptos::server_fn::ServerFnError;
use smallvec::SmallVec;

/// Narrow article-number input with autosuggest dropdown.
///
/// - Submits on Enter / Go: writes `Some(code)` into the shared
///   `article` signal (or `None` when cleared).
/// - Autosuggests as the user types: 200 ms debounce, minimum 3
///   characters after normalisation, capped at 20 results from the
///   server. Suggestions show the article number + manufacturer +
///   product so all-numeric GTINs stay disambiguable.
/// - Clicking a suggestion fills the input and submits.
///
/// SSR renders the input + button only; the dropdown is populated
/// post-hydration when the browser fires the first `input` event.
#[component]
pub fn ArticleBar(article: RwSignal<Option<String>>) -> impl IntoView {
    let draft = RwSignal::new(String::new());
    let suggestions = RwSignal::new(Vec::<ArticleSuggestion>::new());
    let dropdown_open = RwSignal::new(false);

    // Generation counter — every keystroke bumps it; the spawned
    // debounce task only proceeds if its generation is still current
    // when the 200 ms timer fires. This is the classic "cancel in-
    // flight debounce" pattern without needing a real cancellation
    // token.
    let gen = RwSignal::new(0u64);

    let submit = std::sync::Arc::new(move || {
        let v = draft.get();
        let trimmed = v.trim();
        if trimmed.is_empty() {
            article.set(None);
        } else {
            article.set(Some(trimmed.to_string()));
        }
        dropdown_open.set(false);
    });
    let submit_for_btn = submit.clone();
    let submit_for_kbd = submit.clone();

    let on_keydown = move |ev: leptos::ev::KeyboardEvent| match ev.key().as_str() {
        "Enter" => {
            ev.prevent_default();
            submit_for_kbd();
        }
        "Escape" => {
            dropdown_open.set(false);
        }
        _ => {}
    };

    let on_input = move |ev: leptos::ev::Event| {
        let v = event_target_value(&ev);
        draft.set(v.clone());
        // Bump generation. The spawned task below sees the new value
        // when it wakes; any older task notices its `my_gen != gen`
        // and bails out.
        let my_gen = gen.get() + 1;
        gen.set(my_gen);

        // Below the 3-char threshold (after normalisation) we clear
        // the dropdown synchronously — no point waiting 200 ms to
        // show "no suggestions yet". The server-side helper would
        // also short-circuit here, so this is a UX nicety.
        let normalised_len = v.chars().filter(|c| c.is_ascii_alphanumeric()).count();
        if normalised_len < 3 {
            suggestions.set(Vec::new());
            dropdown_open.set(false);
            return;
        }

        // Schedule the debounced server call. SSR target has no event
        // loop to schedule against — this branch is wasm-only.
        #[cfg(target_arch = "wasm32")]
        {
            let prefix = v;
            leptos::task::spawn_local(async move {
                gloo_timers::future::TimeoutFuture::new(200).await;
                // Cancellation check: a newer keystroke would have
                // bumped `gen` past `my_gen` while we were sleeping.
                if gen.get() != my_gen {
                    return;
                }
                match crate::api::suggest_articles(prefix, Some(20)).await {
                    Ok(list) => {
                        // Second check — a later task may have
                        // resolved first; only commit when ours is
                        // still the latest.
                        if gen.get() == my_gen {
                            let any = !list.is_empty();
                            suggestions.set(list);
                            dropdown_open.set(any);
                        }
                    }
                    Err(_) => {
                        if gen.get() == my_gen {
                            suggestions.set(Vec::new());
                            dropdown_open.set(false);
                        }
                    }
                }
            });
        }
    };

    let submit_pick = submit.clone();
    let pick = move |article_str: String| {
        draft.set(article_str);
        submit_pick();
    };

    // Close on blur after a short delay so a click on a row still
    // registers (mousedown lands before blur otherwise eats it).
    let on_blur = move |_| {
        #[cfg(target_arch = "wasm32")]
        {
            leptos::task::spawn_local(async move {
                gloo_timers::future::TimeoutFuture::new(150).await;
                dropdown_open.set(false);
            });
        }
    };

    let on_focus = move |_| {
        // Reopen when the user re-focuses an input that still has
        // queued suggestions.
        if !suggestions.with(|s| s.is_empty()) {
            dropdown_open.set(true);
        }
    };

    view! {
        <div class="article-bar">
            <input
                type="text"
                class="article-input"
                placeholder="Art. #"
                aria-label="Article number"
                autocomplete="off"
                spellcheck="false"
                prop:value=move || draft.get()
                on:input=on_input
                on:keydown=on_keydown
                on:focus=on_focus
                on:blur=on_blur
            />
            <button
                class="article-submit"
                title="Look up article number"
                on:click=move |_| submit_for_btn()
            >
                "Go"
            </button>
            {move || {
                if !dropdown_open.get() {
                    return ().into_any();
                }
                let items = suggestions.get();
                if items.is_empty() {
                    return ().into_any();
                }
                view! {
                    <ul class="article-suggest">
                        {items.into_iter().map(|s| {
                            let article_value = s.article.clone();
                            let pick = pick.clone();
                            view! {
                                <li
                                    class="article-suggest-item"
                                    // mousedown fires before blur, so
                                    // we can mutate state without the
                                    // blur handler closing us first.
                                    on:mousedown=move |ev| {
                                        ev.prevent_default();
                                        pick(article_value.clone());
                                    }
                                >
                                    <span class="article-suggest-code">{s.article}</span>
                                    <span class="article-suggest-meta">
                                        {s.manufacturer} " · " {s.product}
                                    </span>
                                </li>
                            }
                        }).collect_view()}
                    </ul>
                }.into_any()
            }}
        </div>
    }
}

/// Top-of-page free-text input with typed autosuggest + reset
/// button.
///
/// Behaviour:
/// - Each keystroke updates `filter.text` (so existing free-text
///   matching still narrows the result list as you type).
/// - In parallel, a 200 ms debounced `suggest_facets` call populates
///   a dropdown of typed hints (`Manufacturer: AEC Illuminazione`,
///   `IpCode: IP65`, …).
/// - Clicking a hint applies the matching facet filter (e.g. pushes
///   the IP-code id into `filter.ip_code`) AND clears `filter.text`,
///   so the dropdown collapses and the result list is now driven by
///   the typed facet.
/// - Enter or just typing without clicking falls back to the free-
///   text search.
#[component]
pub fn SearchBar(
    filter: RwSignal<Filter>,
    /// True ⇒ Enter commits the current draft as a free-text query.
    /// False ⇒ Enter is ignored (autosuggest dropdown still works).
    /// Controlled by the [`FulltextToggle`] checkbox sibling.
    fulltext_armed: RwSignal<bool>,
    /// Countdown for the auto-disarm. SearchBar refreshes this to
    /// `10` every time the user submits a free-text query so the
    /// armed state stays alive while they iterate.
    disarm_seconds: RwSignal<u32>,
) -> impl IntoView {
    // Local draft — what's visible in the input. Each keystroke
    // updates `draft`, NOT `filter.text`. `filter.text` is only set
    // on Enter (explicit free-text search) AND when `fulltext_armed`
    // is true. Picking a typed suggestion wipes `filter.text` and
    // applies the matching facet instead. This keeps the per-
    // keystroke cost bounded to "fire a debounced suggest_facets"
    // — no full search round-trip while the autosuggest is active.
    let draft = RwSignal::new(String::new());
    let suggestions = RwSignal::new(Vec::<FacetSuggestion>::new());
    let dropdown_open = RwSignal::new(false);
    let gen = RwSignal::new(0u64);

    let on_input = move |ev: leptos::ev::Event| {
        let val = event_target_value(&ev);
        draft.set(val.clone());

        // Debounced facet autosuggest. Min 2 chars (we don't want a
        // dropdown of every manufacturer on the second keystroke).
        let my_gen = gen.get() + 1;
        gen.set(my_gen);

        if val.trim().len() < 2 {
            suggestions.set(Vec::new());
            dropdown_open.set(false);
            return;
        }

        #[cfg(target_arch = "wasm32")]
        {
            let prefix = val;
            leptos::task::spawn_local(async move {
                gloo_timers::future::TimeoutFuture::new(200).await;
                if gen.get() != my_gen {
                    return;
                }
                match crate::api::suggest_facets(prefix, Some(10)).await {
                    Ok(list) => {
                        if gen.get() == my_gen {
                            let any = !list.is_empty();
                            suggestions.set(list);
                            dropdown_open.set(any);
                        }
                    }
                    Err(_) => {
                        if gen.get() == my_gen {
                            suggestions.set(Vec::new());
                            dropdown_open.set(false);
                        }
                    }
                }
            });
        }
    };

    let pick = move |s: FacetSuggestion| {
        // Click on a typed suggestion: apply the facet filter, drop
        // any stale `filter.text` left over from a previous Enter,
        // empty the input box. Result list is now driven purely by
        // facets — fast, no text scan.
        filter.update(|f| {
            apply_facet_suggestion(f, &s);
            f.text = None;
        });
        draft.set(String::new());
        suggestions.set(Vec::new());
        dropdown_open.set(false);
    };

    let on_keydown = move |ev: leptos::ev::KeyboardEvent| match ev.key().as_str() {
        "Enter" => {
            ev.prevent_default();
            if !fulltext_armed.get() {
                // Toggle is off — Enter is a no-op so a stray keystroke
                // can't kick off a corpus-wide text scan.
                return;
            }
            // Commit the current draft as a free-text search and
            // refresh the auto-disarm countdown.
            let v = draft.get();
            let trimmed = v.trim();
            filter.update(|f| {
                f.text = if trimmed.is_empty() {
                    None
                } else {
                    Some(CompactString::from(trimmed))
                };
            });
            disarm_seconds.set(10);
            dropdown_open.set(false);
        }
        "Escape" => {
            dropdown_open.set(false);
        }
        _ => {}
    };

    let on_blur = move |_| {
        #[cfg(target_arch = "wasm32")]
        {
            leptos::task::spawn_local(async move {
                gloo_timers::future::TimeoutFuture::new(150).await;
                dropdown_open.set(false);
            });
        }
    };
    let on_focus = move |_| {
        if !suggestions.with(|s| s.is_empty()) {
            dropdown_open.set(true);
        }
    };

    let reset = move |_| {
        filter.set(Filter::default());
        draft.set(String::new());
        suggestions.set(Vec::new());
        dropdown_open.set(false);
    };

    view! {
        <div class="search-bar">
            <input
                type="text"
                class="search-input"
                class:search-input-armed=move || fulltext_armed.get()
                placeholder=move || if fulltext_armed.get() {
                    "Press Enter — full-text scan over 270k docs"
                } else {
                    "Search luminaires (e.g. IP65, ERCO, Office)…"
                }
                autocomplete="off"
                spellcheck="false"
                on:input=on_input
                on:keydown=on_keydown
                on:focus=on_focus
                on:blur=on_blur
                prop:value=move || draft.get()
            />
            <button class="reset-btn" on:click=reset>Reset</button>
            {move || {
                if !dropdown_open.get() {
                    return ().into_any();
                }
                let items = suggestions.get();
                if items.is_empty() {
                    return ().into_any();
                }
                view! {
                    <ul class="facet-suggest">
                        {items.into_iter().map(|s| {
                            let label = facet_kind_label(&s);
                            let value = s.value().to_string();
                            let count = s.count();
                            let pick = pick.clone();
                            let s_for_click = s.clone();
                            view! {
                                <li
                                    class="facet-suggest-item"
                                    on:mousedown=move |ev| {
                                        ev.prevent_default();
                                        pick(s_for_click.clone());
                                    }
                                >
                                    <span class="facet-suggest-kind">{label}</span>
                                    <span class="facet-suggest-value">{value}</span>
                                    <span class="facet-suggest-count">{count}</span>
                                </li>
                            }
                        }).collect_view()}
                    </ul>
                }.into_any()
            }}
        </div>
    }
}

/// Label shown to the left of the value in the SearchBar dropdown.
fn facet_kind_label(s: &FacetSuggestion) -> &'static str {
    match s {
        FacetSuggestion::Manufacturer { .. } => "Manufacturer",
        FacetSuggestion::IpCode { .. } => "IP code",
        FacetSuggestion::SafetyClass { .. } => "Safety class",
        FacetSuggestion::IkRating { .. } => "IK rating",
        FacetSuggestion::ProductForm { .. } => "Product form",
        FacetSuggestion::Adjustability { .. } => "Adjustability",
        FacetSuggestion::Label { .. } => "Label",
        FacetSuggestion::Application { .. } => "Application",
        FacetSuggestion::LampType { .. } => "Lamp type",
        FacetSuggestion::ControlGearInterface { .. } => "Control gear",
        FacetSuggestion::EmergencyLightingType { .. } => "Emergency",
    }
}

/// Apply a clicked facet suggestion onto the filter. Wires each
/// variant to the right `Filter` field by re-using the same string→id
/// helpers the FacetPanel toggles use, so the chip + the dropdown
/// path resolve to the same id space.
fn apply_facet_suggestion(f: &mut Filter, s: &FacetSuggestion) {
    match s {
        FacetSuggestion::Manufacturer { value, .. } => {
            toggle_manufacturer(f, value);
        }
        FacetSuggestion::IpCode { value, .. } => {
            if let Some(id) = ip_code_from_str(value) {
                if !f.ip_code.contains(&id) {
                    f.ip_code.push(id);
                }
            }
        }
        FacetSuggestion::SafetyClass { value, .. } => {
            if let Some(id) = safety_class_from_str(value) {
                if !f.safety_class.contains(&id) {
                    f.safety_class.push(id);
                }
            }
        }
        FacetSuggestion::IkRating { value, .. } => {
            if let Some(id) = ik_rating_from_str(value) {
                if !f.ik_rating.contains(&id) {
                    f.ik_rating.push(id);
                }
            }
        }
        FacetSuggestion::ProductForm { value, .. } => {
            if let Some(id) = product_form_from_str(value) {
                if !f.product_form.contains(&id) {
                    f.product_form.push(id);
                }
            }
        }
        FacetSuggestion::Adjustability { value, .. } => {
            if let Some(id) = adjustability_from_str(value) {
                if !f.adjustability.contains(&id) {
                    f.adjustability.push(id);
                }
            }
        }
        FacetSuggestion::Label { value, .. } => {
            if let Some(id) = label_from_str(value) {
                if !f.labels.contains(&id) {
                    f.labels.push(id);
                }
            }
        }
        FacetSuggestion::Application { value, .. } => {
            if let Some(id) = application_from_str(value) {
                if !f.applications.contains(&id) {
                    f.applications.push(id);
                }
            }
        }
        FacetSuggestion::LampType { value, .. } => {
            if let Some(id) = lamp_type_from_str(value) {
                if !f.lamp_type.contains(&id) {
                    f.lamp_type.push(id);
                }
            }
        }
        FacetSuggestion::ControlGearInterface { value, .. } => {
            if let Some(id) = control_gear_interface_from_str(value) {
                if !f.control_gear_interfaces.contains(&id) {
                    f.control_gear_interfaces.push(id);
                }
            }
        }
        FacetSuggestion::EmergencyLightingType { value, .. } => {
            if let Some(id) = emergency_lighting_type_from_str(value) {
                if !f.emergency_lighting_type.contains(&id) {
                    f.emergency_lighting_type.push(id);
                }
            }
        }
    }
}

/// One hit row. Shows manufacturer, product, mined keywords, the
/// photometry-present badge, and (when supplied) an "Open in viewer"
/// link.
///
/// `viewer_url` is precomputed server-side from the `ViewerConfig` in
/// context — see `crate::api::fetch_doc`. Passing it as a prop instead
/// of looking up `ViewerConfig` from the browser context is deliberate:
/// the browser hydrate path does not receive `ViewerConfig`, so a
/// `use_context` lookup would diverge from SSR and trigger a hydration
/// mismatch.
#[component]
pub fn HitRow(doc: LuminaireDoc, viewer_url: Option<String>) -> impl IntoView {
    let has_phot = doc.variants.iter().any(|v| v.photometry.is_some());
    let mut kws: Vec<String> = doc.keywords.iter().map(|s| s.to_string()).collect();
    kws.sort();

    // Distinguisher line: prefer product_code (e.g. "O52-380 LED
    // 13000 840 WBA") then GTIN. Many docs in the corpus share the
    // same `product` family name across dozens of SKUs; this is the
    // line that actually tells them apart.
    let sku = doc
        .product_code
        .as_ref()
        .map(|s| s.to_string())
        .or_else(|| doc.gtin.as_ref().map(|s| s.to_string()));

    let variant_count = doc.variants.len();

    // Photometry summary: pull from the first variant that has it.
    // (For multi-variant docs the photometric numbers are usually
    // identical anyway — they describe the same emitter; differences
    // live in mounting / mechanical fields.)
    let phot = doc
        .variants
        .iter()
        .find_map(|v| v.photometry.as_ref())
        .cloned();

    let stats = phot.as_ref().map(|p| render_phot_stats(p));

    // Polar-curve toggle. We don't know server-side whether a given
    // doc has an LDT until we try to read it (the matrix is no longer
    // stored in the index — we ship the raw LDT bytes to the browser
    // on demand instead). Show the button whenever the doc declares
    // any photometry; the click pipeline handles a missing LDT
    // gracefully by rendering "No polar data."
    let _first_variant_id = doc.variants.first().map(|v| v.id.0);
    let id_hex: String = doc.id.0.iter().map(|b| format!("{b:02x}")).collect();

    let polar_open = RwSignal::new(false);
    // Variant currently displayed in the polar/image pane. `None`
    // = doc-level "first photometric file" behaviour (the historic
    // default before the variant-drilldown). `Some(idx)` = that
    // variant's LDT + image. Driven by the per-variant polar
    // buttons in the variant table.
    let current_variant: RwSignal<Option<u16>> = RwSignal::new(None);
    // Variants drilldown table toggle. Separate from `polar_open`
    // because the user might want to see the variant list without
    // opening any polar pane.
    let variants_open = RwSignal::new(false);
    // `Some("")` = fetched, no LDT. `Some(svg)` = render. `None` =
    // not fetched yet.
    let polar_svg: RwSignal<Option<String>> = RwSignal::new(None);
    let polar_loading = RwSignal::new(false);

    // Product image — auto-loads alongside the polar diagram when
    // the user expands a row. `Some(Some(img))` = loaded, render.
    // `Some(None)` = fetched, no image declared. `None` = not
    // fetched yet. Same tri-state shape as `polar_svg`.
    let product_image: RwSignal<Option<Option<crate::api::ProductImage>>> = RwSignal::new(None);
    let image_loading = RwSignal::new(false);

    // Shared fetcher used by both the doc-level polar toggle and the
    // per-variant polar buttons. `variant_index = None` means
    // doc-level. Resets the polar/image signals to "loading" so the
    // expand pane re-fetches when the user switches variants.
    let fire_polar_fetch = {
        let id_hex = id_hex.clone();
        move |variant_index: Option<u16>| {
            current_variant.set(variant_index);
            polar_open.set(true);
            polar_svg.set(None);
            product_image.set(None);
            polar_loading.set(true);
            image_loading.set(true);
            let id_hex_p = id_hex.clone();
            let id_hex_i = id_hex.clone();
            #[cfg(target_arch = "wasm32")]
            {
                leptos::task::spawn_local(async move {
                    let res = crate::api::fetch_ldt_for_variant(id_hex_p, variant_index).await;
                    polar_loading.set(false);
                    match res {
                        Ok(Some(ldt_text)) => {
                            let svg = crate::polar_client::render_polar_from_ldt(&ldt_text, 240.0)
                                .unwrap_or_default();
                            polar_svg.set(Some(svg));
                        }
                        Ok(None) => polar_svg.set(Some(String::new())),
                        Err(_) => polar_svg.set(Some(String::new())),
                    }
                });
                leptos::task::spawn_local(async move {
                    let res =
                        crate::api::fetch_product_image_for_variant(id_hex_i, variant_index).await;
                    image_loading.set(false);
                    match res {
                        Ok(payload) => product_image.set(Some(payload)),
                        Err(_) => product_image.set(Some(None)),
                    }
                });
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let _ = (id_hex_p, id_hex_i);
                polar_loading.set(false);
                image_loading.set(false);
            }
        }
    };
    let fire_polar_fetch = std::sync::Arc::new(fire_polar_fetch);

    let toggle_polar = {
        let fire = fire_polar_fetch.clone();
        move |_| {
            if polar_open.get() {
                polar_open.set(false);
                return;
            }
            // Re-open with whichever variant was last viewed, or the
            // doc-level default. Won't refetch if we already have the
            // same content cached — but `fire_polar_fetch` always
            // resets and re-fetches; cheap on prod with the cache hot.
            fire(current_variant.get_untracked());
        }
    };

    let toggle_variants = move |_| {
        variants_open.update(|v| *v = !*v);
    };

    // Gate: any photometric signal at all (allows the button on
    // doc-mined photometry too; the fetch will resolve gracefully).
    let polar_button_enabled = has_phot;

    // Snapshot of variants for the drilldown table. Cloning here so
    // the view closures don't need to capture &doc.variants; the
    // Vec is small (most family GLDFs have <20 variants).
    let variants_for_table: Vec<gldf_search_schema::doc::VariantDoc> = doc.variants.to_vec();

    view! {
        <li class="hit">
            <div class="hit-header">
                <span class="hit-manufacturer">{doc.manufacturer.to_string()}</span>
                <span class="hit-product">{doc.product.to_string()}</span>
                {(variant_count > 1).then(|| view! {
                    <button
                        class="badge badge-variant-count badge-clickable"
                        title="Show all variants"
                        on:click=toggle_variants
                    >
                        {variant_count} " variants"
                        {move || if variants_open.get() { " ▴" } else { " ▾" }}
                    </button>
                })}
                {(!has_phot).then(|| view! {
                    <span class="badge badge-missing">"no photometry"</span>
                })}
                {polar_button_enabled.then(|| view! {
                    <button
                        class="polar-toggle"
                        title="Show polar curve"
                        on:click=toggle_polar
                    >
                        {move || if polar_open.get() { "↑ Hide polar" } else { "↓ Polar" }}
                    </button>
                })}
                {viewer_url.map(|href| view! {
                    <a class="viewer-link" href=href target="_blank" rel="noopener">
                        "Open in viewer ↗"
                    </a>
                })}
            </div>
            {sku.map(|code| view! {
                <div class="hit-sku">{code}</div>
            })}
            {stats}
            {move || {
                if !polar_open.get() {
                    return ().into_any();
                }
                // Image + polar render side-by-side inside the
                // expand pane. Each side independently loads/empties.
                let polar_view = if polar_loading.get() {
                    view! { <div class="hit-polar"><p>"Rendering polar…"</p></div> }.into_any()
                } else {
                    polar_svg.with(|s| match s {
                        Some(svg) if !svg.is_empty() => view! {
                            <div class="hit-polar" inner_html=svg.clone()></div>
                        }.into_any(),
                        Some(_) => view! {
                            <div class="hit-polar"><p class="empty">"No polar data."</p></div>
                        }.into_any(),
                        None => ().into_any(),
                    })
                };
                let image_view = if image_loading.get() {
                    view! { <div class="hit-image"><p>"Loading image…"</p></div> }.into_any()
                } else {
                    product_image.with(|i| match i {
                        Some(Some(img)) => {
                            let src = format!("data:{};base64,{}", img.mime, img.data_base64);
                            view! {
                                <div class="hit-image">
                                    <img src=src alt="Product picture"/>
                                </div>
                            }.into_any()
                        }
                        Some(None) => ().into_any(), // no image declared — silent
                        None => ().into_any(),
                    })
                };
                view! {
                    <div class="hit-expand">
                        {move || current_variant.get().map(|idx| view! {
                            <p class="hit-expand-label">
                                "Variant " {idx + 1} " of " {variant_count}
                            </p>
                        })}
                        <div class="hit-expand-row">
                            {image_view}
                            {polar_view}
                        </div>
                    </div>
                }
                .into_any()
            }}
            {move || {
                if !variants_open.get() {
                    return ().into_any();
                }
                let fire = fire_polar_fetch.clone();
                let rows = variants_for_table.iter().enumerate().map(|(i, v)| {
                    let idx = i as u16;
                    let name = v.name.to_string();
                    let phot = v.photometry.clone();
                    let mounting = format!("{:?} / {:?}", v.mounting_place, v.mounting_type);
                    let depth = v.recessed_depth_mm
                        .map(|d| format!("{} mm", d))
                        .unwrap_or_default();
                    let is_active = move || current_variant.get() == Some(idx);
                    let fire = fire.clone();
                    let on_click = move |_| fire(Some(idx));
                    let stats = phot.map(|p| variant_stats_inline(&p));
                    view! {
                        <tr class:variant-row-active=is_active>
                            <td class="v-idx">{idx + 1}</td>
                            <td class="v-name">{name}</td>
                            <td class="v-stats">{stats}</td>
                            <td class="v-mounting">{mounting}</td>
                            <td class="v-depth">{depth}</td>
                            <td class="v-action">
                                <button
                                    class="polar-toggle"
                                    title="Show polar for this variant"
                                    on:click=on_click
                                >
                                    "↓ Polar"
                                </button>
                            </td>
                        </tr>
                    }
                }).collect_view();
                view! {
                    <div class="hit-variants">
                        <table>
                            <thead>
                                <tr>
                                    <th>"#"</th>
                                    <th>"Name"</th>
                                    <th>"Photometry"</th>
                                    <th>"Mounting"</th>
                                    <th>"Depth"</th>
                                    <th></th>
                                </tr>
                            </thead>
                            <tbody>{rows}</tbody>
                        </table>
                    </div>
                }
                .into_any()
            }}
            {(!kws.is_empty()).then(|| view! {
                <ul class="hit-keywords">
                    {kws.into_iter().map(|k| view! { <li>{k}</li> }).collect_view()}
                </ul>
            })}
        </li>
    }
}

/// One-line photometry summary for the variant-drilldown table.
/// Same numeric fields as `render_phot_stats` but rendered as a
/// compact string (the table column is narrower than the main
/// hit-row stats line).
fn variant_stats_inline(p: &gldf_search_schema::doc::PhotometryStats) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(5);
    if let Some(f) = p.flux_lm {
        parts.push(format!("{} lm", f as u32));
    }
    if let Some(pw) = p.power_w {
        parts.push(format!("{} W", pw as u32));
    }
    if let Some(cct) = p.cct_k {
        parts.push(format!("{} K", cct));
    }
    if let Some(b) = p.beam_deg {
        parts.push(format!("{}°", b as u32));
    }
    parts.join(" · ")
}

/// One-line photometry summary rendered as a flex row of small
/// `<dl>`-style pairs. Each value is `None`-tolerant — missing
/// numbers simply don't render. Output looks like:
///
/// ```text
/// 2680 lm · 18 W · 3000 K · Ra 80 · 60°
/// ```
///
/// The aim is "enough context to tell two SKU rows apart at a
/// glance" — not a full datasheet. Detailed photometry belongs on a
/// hover / detail page (future work).
fn render_phot_stats(p: &gldf_search_schema::doc::PhotometryStats) -> impl leptos::IntoView {
    let mut parts: Vec<(String, &'static str)> = Vec::with_capacity(5);
    if let Some(v) = p.flux_lm {
        parts.push((format!("{}", v.round() as i64), "lm"));
    }
    if let Some(v) = p.power_w {
        parts.push((format!("{}", v.round() as i64), "W"));
    }
    if let Some(v) = p.cct_k {
        parts.push((format!("{v}"), "K"));
    }
    if let Some(v) = p.cri_ra {
        parts.push((format!("Ra {v}"), ""));
    }
    if let Some(v) = p.beam_deg {
        parts.push((format!("{}°", v.round() as i64), ""));
    }
    if parts.is_empty() {
        return ().into_any();
    }
    view! {
        <div class="hit-stats">
            {parts.into_iter().map(|(n, unit)| {
                view! {
                    <span class="hit-stat">
                        <span class="hit-stat-num">{n}</span>
                        {(!unit.is_empty()).then(|| view! {
                            <span class="hit-stat-unit">{" "} {unit}</span>
                        })}
                    </span>
                }
            }).collect_view()}
        </div>
    }
    .into_any()
}

/// Slim-payload variant of `render_phot_stats`. Same visual; reads
/// from `PhotometrySlim` so VariantRow doesn't need the full
/// `PhotometryStats` shipped over the wire.
fn render_phot_stats_slim(p: &crate::api::PhotometrySlim) -> impl leptos::IntoView {
    let mut parts: Vec<(String, &'static str)> = Vec::with_capacity(5);
    if let Some(v) = p.flux_lm {
        parts.push((format!("{}", v.round() as i64), "lm"));
    }
    if let Some(v) = p.power_w {
        parts.push((format!("{}", v.round() as i64), "W"));
    }
    if let Some(v) = p.cct_k {
        parts.push((format!("{v}"), "K"));
    }
    if let Some(v) = p.cri_ra {
        parts.push((format!("Ra {v}"), ""));
    }
    if let Some(v) = p.beam_deg {
        parts.push((format!("{}°", v.round() as i64), ""));
    }
    if parts.is_empty() {
        return ().into_any();
    }
    view! {
        <div class="hit-stats">
            {parts.into_iter().map(|(n, unit)| {
                view! {
                    <span class="hit-stat">
                        <span class="hit-stat-num">{n}</span>
                        {(!unit.is_empty()).then(|| view! {
                            <span class="hit-stat-unit">{" "} {unit}</span>
                        })}
                    </span>
                }
            }).collect_view()}
        </div>
    }
    .into_any()
}

/// Facet column. Renders the most useful facet groups; clicking a
/// value toggles it in the filter so the user can drill in/out.
///
/// Order: the structured (closed-enum) facets lead — they're shorter
/// and more discoverable. Manufacturer is the long, noisy list and
/// sits near the bottom, just above the photometry-presence triage.
#[component]
pub fn FacetPanel(
    filter: RwSignal<Filter>,
    counts: FacetCounts,
    /// When `true`, the user is on a per-manufacturer subdomain and
    /// the Manufacturer facet should not render — every facet click
    /// against another manufacturer would be silently overridden by
    /// the server-side scope anyway, so the affordance is just a
    /// footgun. The corresponding `<FilterChips>` chip is also
    /// suppressed via the same flag.
    #[prop(optional)]
    manufacturer_locked: bool,
) -> impl IntoView {
    view! {
        <aside class="facets">
            <ApplicationTree
                values=counts.applications.into_iter().map(|c| (c.value, c.count)).collect()
                filter=filter
            />
            <FacetGroup
                name="Light distribution"
                values=counts.light_distribution.into_iter().map(|c| (c.value, c.count)).collect()
                is_selected=Box::new(move |v| {
                    if let Some(id) = light_distribution_from_str(v) {
                        filter.with(|f| f.light_distribution.contains(&id))
                    } else {
                        false
                    }
                })
                on_toggle=Box::new(move |v| {
                    if let Some(id) = light_distribution_from_str(v) {
                        filter.update(|f| toggle_id(&mut f.light_distribution, id));
                    }
                })
                glyph=crate::glyphs::lvk_glyph
            />
            <FacetGroup
                name="Mounting place"
                values=counts.mounting_place.into_iter().map(|c| (c.value, c.count)).collect()
                is_selected=Box::new(move |v| {
                    mounting_place_from_str(v)
                        .map(|g| filter.with(|f| f.mounting_place.contains(&g)))
                        .unwrap_or(false)
                })
                on_toggle=Box::new(move |v| {
                    if let Some(g) = mounting_place_from_str(v) {
                        filter.update(|f| toggle_in_vec(&mut f.mounting_place, g));
                    }
                })
            />
            <FacetGroup
                name="Mounting type"
                values=counts.mounting_type.into_iter().map(|c| (c.value, c.count)).collect()
                is_selected=Box::new(move |v| {
                    mounting_type_from_str(v)
                        .map(|g| filter.with(|f| f.mounting_type.contains(&g)))
                        .unwrap_or(false)
                })
                on_toggle=Box::new(move |v| {
                    if let Some(g) = mounting_type_from_str(v) {
                        filter.update(|f| toggle_in_vec(&mut f.mounting_type, g));
                    }
                })
            />
            <FacetGroup
                name="Product form"
                values=counts.product_form.into_iter().map(|c| (c.value, c.count)).collect()
                is_selected=Box::new(move |v| {
                    if let Some(id) = product_form_from_str(v) {
                        filter.with(|f| f.product_form.contains(&id))
                    } else {
                        false
                    }
                })
                on_toggle=Box::new(move |v| {
                    if let Some(id) = product_form_from_str(v) {
                        filter.update(|f| toggle_id(&mut f.product_form, id));
                    }
                })
            />
            <FacetGroup
                name="IP code"
                values=counts.ip_code.into_iter().map(|c| (c.value, c.count)).collect()
                is_selected=Box::new(move |v| {
                    if let Some(id) = ip_code_from_str(v) {
                        filter.with(|f| f.ip_code.contains(&id))
                    } else {
                        false
                    }
                })
                on_toggle=Box::new(move |v| {
                    if let Some(id) = ip_code_from_str(v) {
                        filter.update(|f| toggle_id(&mut f.ip_code, id));
                    }
                })
            />
            <FacetGroup
                name="Safety class"
                values=counts.safety_class.into_iter().map(|c| (c.value, c.count)).collect()
                is_selected=Box::new(move |v| {
                    if let Some(id) = safety_class_from_str(v) {
                        filter.with(|f| f.safety_class.contains(&id))
                    } else {
                        false
                    }
                })
                on_toggle=Box::new(move |v| {
                    if let Some(id) = safety_class_from_str(v) {
                        filter.update(|f| toggle_id(&mut f.safety_class, id));
                    }
                })
            />
            <FacetGroup
                name="IK rating"
                values=counts.ik_rating.into_iter().map(|c| (c.value, c.count)).collect()
                is_selected=Box::new(move |v| {
                    if let Some(id) = ik_rating_from_str(v) {
                        filter.with(|f| f.ik_rating.contains(&id))
                    } else {
                        false
                    }
                })
                on_toggle=Box::new(move |v| {
                    if let Some(id) = ik_rating_from_str(v) {
                        filter.update(|f| toggle_id(&mut f.ik_rating, id));
                    }
                })
            />
            <FacetGroup
                name="Adjustability"
                values=counts.adjustability.into_iter().map(|c| (c.value, c.count)).collect()
                is_selected=Box::new(move |v| {
                    if let Some(id) = adjustability_from_str(v) {
                        filter.with(|f| f.adjustability.contains(&id))
                    } else {
                        false
                    }
                })
                on_toggle=Box::new(move |v| {
                    if let Some(id) = adjustability_from_str(v) {
                        filter.update(|f| toggle_id(&mut f.adjustability, id));
                    }
                })
            />
            <FacetGroup
                name="Labels"
                values=counts.labels.into_iter().map(|c| (c.value, c.count)).collect()
                is_selected=Box::new(move |v| {
                    if let Some(id) = label_from_str(v) {
                        filter.with(|f| f.labels.contains(&id))
                    } else {
                        false
                    }
                })
                on_toggle=Box::new(move |v| {
                    if let Some(id) = label_from_str(v) {
                        filter.update(|f| toggle_id(&mut f.labels, id));
                    }
                })
            />
            {(!manufacturer_locked).then(|| view! {
                <FacetGroup
                    name="Manufacturer"
                    values=counts.manufacturer.into_iter().map(|c| (c.value, c.count)).collect()
                    is_selected=Box::new(move |v| {
                        filter.with(|f| {
                            f.manufacturer
                                .as_ref()
                                .map(|list| list.iter().any(|m| m == v))
                                .unwrap_or(false)
                        })
                    })
                    on_toggle=Box::new(move |v| {
                        let val = v.to_string();
                        filter.update(|f| toggle_manufacturer(f, &val));
                    })
                />
            })}
            <FacetGroup
                name="Lamp type"
                values=counts.lamp_type.into_iter().map(|c| (c.value, c.count)).collect()
                is_selected=Box::new(move |v| {
                    if let Some(id) = lamp_type_from_str(v) {
                        filter.with(|f| f.lamp_type.contains(&id))
                    } else { false }
                })
                on_toggle=Box::new(move |v| {
                    if let Some(id) = lamp_type_from_str(v) {
                        filter.update(|f| toggle_id(&mut f.lamp_type, id));
                    }
                })
            />
            <FacetGroup
                name="Control gear interface"
                values=counts.control_gear_interfaces.into_iter().map(|c| (c.value, c.count)).collect()
                is_selected=Box::new(move |v| {
                    if let Some(id) = control_gear_interface_from_str(v) {
                        filter.with(|f| f.control_gear_interfaces.contains(&id))
                    } else { false }
                })
                on_toggle=Box::new(move |v| {
                    if let Some(id) = control_gear_interface_from_str(v) {
                        filter.update(|f| toggle_id(&mut f.control_gear_interfaces, id));
                    }
                })
            />
            <FacetGroup
                name="Emergency lighting"
                values=counts.emergency_lighting_type.into_iter().map(|c| (c.value, c.count)).collect()
                is_selected=Box::new(move |v| {
                    if v == gldf_search_index::UNSPECIFIED_EMERGENCY {
                        // Synthetic bucket → flagged by the tristate.
                        filter.with(|f| f.has_emergency_lighting == Some(true)
                            && f.emergency_lighting_type.is_empty())
                    } else if let Some(id) = emergency_lighting_type_from_str(v) {
                        filter.with(|f| f.emergency_lighting_type.contains(&id))
                    } else { false }
                })
                on_toggle=Box::new(move |v| {
                    if v == gldf_search_index::UNSPECIFIED_EMERGENCY {
                        // Toggle the tristate. When activating, also
                        // clear any typed subtype so the user sees the
                        // "emergency, no subtype declared" cohort —
                        // a typed subtype would over-narrow.
                        filter.update(|f| {
                            if f.has_emergency_lighting == Some(true)
                                && f.emergency_lighting_type.is_empty()
                            {
                                f.has_emergency_lighting = None;
                            } else {
                                f.has_emergency_lighting = Some(true);
                                f.emergency_lighting_type.clear();
                            }
                        });
                    } else if let Some(id) = emergency_lighting_type_from_str(v) {
                        filter.update(|f| toggle_id(&mut f.emergency_lighting_type, id));
                    }
                })
            />
            <FacetGroup
                name="Has photometry"
                values=counts.has_photometry.into_iter().map(|c| (c.value, c.count)).collect()
                is_selected=Box::new(move |v| {
                    filter.with(|f| {
                        matches!(
                            (f.has_photometry, v),
                            (Some(true), "present") | (Some(false), "missing")
                        )
                    })
                })
                on_toggle=Box::new(move |v| {
                    filter.update(|f| {
                        let want = match v {
                            "present" => Some(true),
                            "missing" => Some(false),
                            _ => return,
                        };
                        f.has_photometry = if f.has_photometry == want { None } else { want };
                    });
                })
            />
            <FacetGroup
                name="Has 3D model"
                values=counts.has_3d.into_iter().map(|c| (c.value, c.count)).collect()
                is_selected=Box::new(move |v| {
                    filter.with(|f| {
                        matches!(
                            (f.has_3d, v),
                            (Some(true), "present") | (Some(false), "missing")
                        )
                    })
                })
                on_toggle=Box::new(move |v| {
                    filter.update(|f| {
                        let want = match v {
                            "present" => Some(true),
                            "missing" => Some(false),
                            _ => return,
                        };
                        f.has_3d = if f.has_3d == want { None } else { want };
                    });
                })
            />
        </aside>
    }
}

/// Explicit opt-in for the slow full-text scan. Renders a checkbox
/// plus an inline notice / countdown.
///
/// State semantics:
/// - Default off. While off, the [`SearchBar`] ignores Enter.
/// - User ticks the box → `fulltext_armed = true`, `disarm_seconds`
///   starts at 10 and a background task decrements it each second.
/// - At 0 seconds the box auto-unticks and `fulltext_armed` flips
///   back to false. Any in-flight server-side query is **not**
///   cancelled (we can't — see the comment in [`SearchPage`]); it
///   simply finishes and the result is displayed. The user can no
///   longer launch a new full-text scan without re-arming.
/// - Every successful submission (handled inside `SearchBar`)
///   resets `disarm_seconds` back to 10, so iterating queries
///   stays armed indefinitely.
/// - Unticking manually wipes `filter.text` and the result list
///   returns to facet-driven view.
#[component]
pub fn FulltextToggle(
    fulltext_armed: RwSignal<bool>,
    disarm_seconds: RwSignal<u32>,
) -> impl IntoView {
    // Drives the per-second tick. Spawned once when the user arms
    // the toggle; it exits as soon as `fulltext_armed` flips back
    // to false or the countdown hits zero.
    let on_change = move |ev: leptos::ev::Event| {
        let checked = leptos::prelude::event_target_checked(&ev);
        if checked {
            fulltext_armed.set(true);
            disarm_seconds.set(10);
            #[cfg(target_arch = "wasm32")]
            {
                leptos::task::spawn_local(async move {
                    // Decrement every second until either the user
                    // unticks the box or the count hits zero.
                    loop {
                        gloo_timers::future::TimeoutFuture::new(1000).await;
                        if !fulltext_armed.get() {
                            // User unticked manually; nothing to do.
                            return;
                        }
                        let remaining = disarm_seconds.get();
                        if remaining <= 1 {
                            // Auto-disarm. Wipe the text filter so
                            // the result list returns to the facet
                            // view immediately on next signal tick.
                            fulltext_armed.set(false);
                            disarm_seconds.set(0);
                            return;
                        }
                        disarm_seconds.set(remaining - 1);
                    }
                });
            }
        } else {
            // Manual un-tick. Reset everything.
            fulltext_armed.set(false);
            disarm_seconds.set(0);
        }
    };

    view! {
        <div class="fulltext-toggle" class:fulltext-toggle-armed=move || fulltext_armed.get()>
            <label class="fulltext-checkbox">
                <input
                    type="checkbox"
                    prop:checked=move || fulltext_armed.get()
                    on:change=on_change
                />
                <span class="fulltext-label-text">
                    "Enable full-text search "
                    <span class="fulltext-warning">"(slow — scans every doc)"</span>
                </span>
            </label>
            {move || {
                if !fulltext_armed.get() {
                    return ().into_any();
                }
                let s = disarm_seconds.get();
                view! {
                    <span class="fulltext-countdown">
                        "Auto-disabling in " {s} "s — press Enter to scan"
                    </span>
                }.into_any()
            }}
        </div>
    }
}

/// Row of removable chips above the hit list, one per active filter
/// value. Clicking the × on a chip clears that single filter value;
/// the chip set re-renders whenever the filter signal changes.
///
/// Renders nothing when no filter is active — the row is invisible
/// until the user actually narrows the query.
#[component]
pub fn FilterChips(
    filter: RwSignal<Filter>,
    /// See `FacetPanel::manufacturer_locked` — when set, the
    /// Manufacturer chip is hidden because the subdomain enforces it
    /// server-side and clicking ✕ would be ignored.
    #[prop(optional)]
    manufacturer_locked: bool,
) -> impl IntoView {
    let chips = move || {
        filter.with(|f| {
            let mut chips = collect_chips(f);
            if manufacturer_locked {
                chips.retain(|c| c.kind != ChipKind::Manufacturer);
            }
            chips
        })
    };

    view! {
        <div class="filter-chips" class:filter-chips-empty=move || chips().is_empty()>
            {move || {
                let items = chips();
                if items.is_empty() {
                    return ().into_any();
                }
                items
                    .into_iter()
                    .map(|chip| {
                        let kind = chip.kind;
                        let value = chip.value.clone();
                        let label = chip.label.clone();
                        view! {
                            <button
                                class="filter-chip"
                                title="Remove this filter"
                                on:click=move |_| {
                                    let v = value.clone();
                                    filter.update(|f| remove_chip(f, kind, &v));
                                }
                            >
                                <span class="filter-chip-label">{label}</span>
                                <span class="filter-chip-x">"×"</span>
                            </button>
                        }
                    })
                    .collect_view()
                    .into_any()
            }}
        </div>
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChipKind {
    Text,
    Manufacturer,
    Application,
    Label,
    IpCode,
    SafetyClass,
    IkRating,
    ProductForm,
    Adjustability,
    MountingPlace,
    MountingType,
    LightDistribution,
    HasPhotometry,
    Has3d,
    LampType,
    ControlGearInterface,
    EmergencyLightingType,
    FluxLm,
    PowerW,
    Efficacy,
    CctK,
    CriMin,
    BeamDeg,
    RecessedDepthMm,
}

#[derive(Clone, Debug)]
struct Chip {
    kind: ChipKind,
    /// Identifier used when removing the chip — for facet-id chips this
    /// is the canonical XSD string (re-parsed by `*_from_str`); for the
    /// singleton filters (text, has_photometry, ranges) it is unused.
    value: String,
    /// Display string shown in the chip.
    label: String,
}

fn collect_chips(f: &Filter) -> Vec<Chip> {
    let mut out: Vec<Chip> = Vec::new();

    if let Some(t) = &f.text {
        out.push(Chip {
            kind: ChipKind::Text,
            value: String::new(),
            label: format!("Text: {t}"),
        });
    }

    if let Some(list) = &f.manufacturer {
        for m in list {
            out.push(Chip {
                kind: ChipKind::Manufacturer,
                value: m.to_string(),
                label: format!("Manufacturer: {m}"),
            });
        }
    }

    push_id_chips(
        &mut out,
        ChipKind::Application,
        "Application",
        &f.applications,
        application_str,
    );
    push_id_chips(&mut out, ChipKind::Label, "Label", &f.labels, label_str);
    push_id_chips(&mut out, ChipKind::IpCode, "IP", &f.ip_code, ip_code_str);
    push_id_chips(
        &mut out,
        ChipKind::SafetyClass,
        "Safety class",
        &f.safety_class,
        safety_class_str,
    );
    push_id_chips(
        &mut out,
        ChipKind::IkRating,
        "IK",
        &f.ik_rating,
        ik_rating_str,
    );
    push_id_chips(
        &mut out,
        ChipKind::ProductForm,
        "Form",
        &f.product_form,
        product_form_str,
    );
    push_id_chips(
        &mut out,
        ChipKind::Adjustability,
        "Adjustability",
        &f.adjustability,
        adjustability_str,
    );
    push_id_chips(
        &mut out,
        ChipKind::LightDistribution,
        "Distribution",
        &f.light_distribution,
        light_distribution_str,
    );
    push_id_chips(
        &mut out,
        ChipKind::LampType,
        "Lamp",
        &f.lamp_type,
        lamp_type_str,
    );
    push_id_chips(
        &mut out,
        ChipKind::ControlGearInterface,
        "Control gear",
        &f.control_gear_interfaces,
        control_gear_interface_str,
    );
    push_id_chips(
        &mut out,
        ChipKind::EmergencyLightingType,
        "Emergency",
        &f.emergency_lighting_type,
        emergency_lighting_type_str,
    );

    for g in &f.mounting_place {
        out.push(Chip {
            kind: ChipKind::MountingPlace,
            value: format!("{g:?}"),
            label: format!("Mounting place: {g:?}"),
        });
    }
    for g in &f.mounting_type {
        out.push(Chip {
            kind: ChipKind::MountingType,
            value: format!("{g:?}"),
            label: format!("Mounting type: {g:?}"),
        });
    }

    if let Some(present) = f.has_photometry {
        out.push(Chip {
            kind: ChipKind::HasPhotometry,
            value: String::new(),
            label: if present {
                "Photometry: present".to_string()
            } else {
                "Photometry: missing".to_string()
            },
        });
    }
    if let Some(present) = f.has_3d {
        out.push(Chip {
            kind: ChipKind::Has3d,
            value: String::new(),
            label: if present {
                "3D model: present".to_string()
            } else {
                "3D model: missing".to_string()
            },
        });
    }
    // Emergency-lighting tristate chip — only emitted when the user
    // explicitly picked the synthetic "(unspecified)" bucket
    // (`has_emergency_lighting = Some(true)` with no typed subtype).
    // Typed subtypes already render their own chips via
    // `EmergencyLightingType` below, so we'd double-count otherwise.
    if f.has_emergency_lighting == Some(true) && f.emergency_lighting_type.is_empty() {
        out.push(Chip {
            kind: ChipKind::EmergencyLightingType,
            value: gldf_search_index::UNSPECIFIED_EMERGENCY.to_string(),
            label: "Emergency: (unspecified)".to_string(),
        });
    }

    if let Some(r) = &f.flux_lm {
        out.push(Chip {
            kind: ChipKind::FluxLm,
            value: String::new(),
            label: format!("Flux: {}", fmt_range_f32(r)),
        });
    }
    if let Some(r) = &f.power_w {
        out.push(Chip {
            kind: ChipKind::PowerW,
            value: String::new(),
            label: format!("Power: {}", fmt_range_f32(r)),
        });
    }
    if let Some(r) = &f.efficacy {
        out.push(Chip {
            kind: ChipKind::Efficacy,
            value: String::new(),
            label: format!("Efficacy: {}", fmt_range_f32(r)),
        });
    }
    if let Some(r) = &f.cct_k {
        out.push(Chip {
            kind: ChipKind::CctK,
            value: String::new(),
            label: format!("CCT: {}–{} K", r.start(), r.end()),
        });
    }
    if let Some(min) = f.cri_min {
        out.push(Chip {
            kind: ChipKind::CriMin,
            value: String::new(),
            label: format!("CRI ≥ {min}"),
        });
    }
    if let Some(r) = &f.beam_deg {
        out.push(Chip {
            kind: ChipKind::BeamDeg,
            value: String::new(),
            label: format!("Beam: {}", fmt_range_f32(r)),
        });
    }
    if let Some(r) = &f.recessed_depth_mm {
        out.push(Chip {
            kind: ChipKind::RecessedDepthMm,
            value: String::new(),
            label: format!("Recessed depth: {}–{} mm", r.start(), r.end()),
        });
    }

    out
}

fn push_id_chips<const N: usize>(
    out: &mut Vec<Chip>,
    kind: ChipKind,
    prefix: &str,
    slot: &SmallVec<[u8; N]>,
    to_str: fn(u8) -> Option<&'static str>,
) {
    for &id in slot {
        if let Some(s) = to_str(id) {
            out.push(Chip {
                kind,
                value: s.to_string(),
                label: format!("{prefix}: {s}"),
            });
        }
    }
}

fn fmt_range_f32(r: &core::ops::RangeInclusive<f32>) -> String {
    let lo = *r.start();
    let hi = *r.end();
    let lo_s = if lo == f32::NEG_INFINITY {
        "*".to_string()
    } else {
        format!("{lo}")
    };
    let hi_s = if hi == f32::INFINITY {
        "*".to_string()
    } else {
        format!("{hi}")
    };
    format!("{lo_s}–{hi_s}")
}

fn remove_chip(f: &mut Filter, kind: ChipKind, value: &str) {
    match kind {
        ChipKind::Text => f.text = None,
        ChipKind::Manufacturer => {
            if let Some(list) = &mut f.manufacturer {
                if let Some(pos) = list.iter().position(|m| m == value) {
                    list.swap_remove(pos);
                }
                if list.is_empty() {
                    f.manufacturer = None;
                }
            }
        }
        ChipKind::Application => remove_id(&mut f.applications, value, application_from_str),
        ChipKind::Label => remove_id(&mut f.labels, value, label_from_str),
        ChipKind::IpCode => remove_id(&mut f.ip_code, value, ip_code_from_str),
        ChipKind::SafetyClass => remove_id(&mut f.safety_class, value, safety_class_from_str),
        ChipKind::IkRating => remove_id(&mut f.ik_rating, value, ik_rating_from_str),
        ChipKind::ProductForm => remove_id(&mut f.product_form, value, product_form_from_str),
        ChipKind::Adjustability => remove_id(&mut f.adjustability, value, adjustability_from_str),
        ChipKind::LightDistribution => remove_id(
            &mut f.light_distribution,
            value,
            light_distribution_from_str,
        ),
        ChipKind::MountingPlace => {
            if let Some(g) = mounting_place_from_str(value) {
                if let Some(pos) = f.mounting_place.iter().position(|x| *x == g) {
                    f.mounting_place.swap_remove(pos);
                }
            }
        }
        ChipKind::MountingType => {
            if let Some(g) = mounting_type_from_str(value) {
                if let Some(pos) = f.mounting_type.iter().position(|x| *x == g) {
                    f.mounting_type.swap_remove(pos);
                }
            }
        }
        ChipKind::HasPhotometry => f.has_photometry = None,
        ChipKind::Has3d => f.has_3d = None,
        ChipKind::LampType => remove_id(&mut f.lamp_type, value, lamp_type_from_str),
        ChipKind::ControlGearInterface => remove_id(
            &mut f.control_gear_interfaces,
            value,
            control_gear_interface_from_str,
        ),
        ChipKind::EmergencyLightingType => {
            // The chip's `value` is either a canonical XSD string
            // (a typed subtype) or the synthetic UNSPECIFIED marker.
            if value == gldf_search_index::UNSPECIFIED_EMERGENCY {
                // Clear the tristate that the synthetic bucket sets.
                if f.emergency_lighting_type.is_empty() {
                    f.has_emergency_lighting = None;
                }
            } else {
                remove_id(
                    &mut f.emergency_lighting_type,
                    value,
                    emergency_lighting_type_from_str,
                );
            }
        }
        ChipKind::FluxLm => f.flux_lm = None,
        ChipKind::PowerW => f.power_w = None,
        ChipKind::Efficacy => f.efficacy = None,
        ChipKind::CctK => f.cct_k = None,
        ChipKind::CriMin => f.cri_min = None,
        ChipKind::BeamDeg => f.beam_deg = None,
        ChipKind::RecessedDepthMm => f.recessed_depth_mm = None,
    }
}

fn remove_id<const N: usize>(
    slot: &mut SmallVec<[u8; N]>,
    value: &str,
    from_str: fn(&str) -> Option<u8>,
) {
    if let Some(id) = from_str(value) {
        if let Some(pos) = slot.iter().position(|x| *x == id) {
            slot.swap_remove(pos);
        }
    }
}

/// Numeric range inputs for the 6 photometric Filter fields. Each
/// row renders a dual-handle slider whose left/right rails are
/// computed from the current filter's result set (with that axis
/// *relaxed*) — so the user never drags into a dead zone with zero
/// matches. A collapsible "enter exact value" disclosure exposes
/// the precise number-input pair for power users.
///
/// The seventh control (`cri_min`) is a single threshold, not a
/// range — it keeps its existing single-handle UI.
#[component]
pub fn NumericRangeInputs(
    filter: RwSignal<Filter>,
    bounds: Resource<Result<RangeBounds, ServerFnError>>,
) -> impl IntoView {
    // Mirror the Resource into a plain `RwSignal<Option<RangeBounds>>`
    // via an Effect. This is needed because Leptos 0.8's `Resource`
    // on the hydrate side will only resolve inside a `Suspense` /
    // `Transition` / `Effect` scope — and our per-axis `Memo`s aren't
    // any of those. Without this mirror, `bounds.get()` reads from
    // Memos returned `None` forever, so the sliders rendered disabled
    // even though the wire payload had valid bounds (confirmed via
    // DevTools).
    //
    // The Effect IS a valid read scope, so this copy fires on
    // resource resolution and on every subsequent filter-driven
    // refresh. The slider Memos then read `current_bounds.get()`
    // (a plain signal read, no scope restriction).
    let current_bounds: RwSignal<Option<RangeBounds>> = RwSignal::new(None);
    Effect::new(move |_| {
        if let Some(Ok(rb)) = bounds.get() {
            current_bounds.set(Some(rb));
        }
    });

    // Subscribe once per axis. Each accessor returns the
    // axis-specific bounds for the *current* resource value, or
    // `None` while the resource is still loading / on error / when
    // no variant in the relaxed result set carries that field.
    let flux_b = Memo::new(move |_| current_bounds.get().and_then(|b| b.flux_lm));
    let power_b = Memo::new(move |_| current_bounds.get().and_then(|b| b.power_w));
    let efficacy_b = Memo::new(move |_| current_bounds.get().and_then(|b| b.efficacy));
    let cct_b = Memo::new(move |_| current_bounds.get().and_then(|b| b.cct_k));
    let beam_b = Memo::new(move |_| current_bounds.get().and_then(|b| b.beam_deg));
    let depth_b = Memo::new(move |_| current_bounds.get().and_then(|b| b.recessed_depth_mm));

    view! {
        <section class="ranges">
            <h4>"Photometry"</h4>
            <DualSliderF32
                label="Luminous flux (lm)"
                step=1.0
                bounds=flux_b
                get=Box::new(move || filter.with(|f| f.flux_lm.clone()))
                set=Box::new(move |r| filter.update(|f| f.flux_lm = r))
            />
            <DualSliderF32
                label="Power (W)"
                step=1.0
                bounds=power_b
                get=Box::new(move || filter.with(|f| f.power_w.clone()))
                set=Box::new(move |r| filter.update(|f| f.power_w = r))
            />
            <DualSliderF32
                label="Efficacy (lm/W)"
                step=0.1
                bounds=efficacy_b
                get=Box::new(move || filter.with(|f| f.efficacy.clone()))
                set=Box::new(move |r| filter.update(|f| f.efficacy = r))
            />
            <DualSliderU16
                label="CCT (K)"
                step=100
                bounds=cct_b
                get=Box::new(move || filter.with(|f| f.cct_k.clone()))
                set=Box::new(move |r| filter.update(|f| f.cct_k = r))
            />
            <DualSliderF32
                label="Beam angle (°)"
                step=1.0
                bounds=beam_b
                get=Box::new(move || filter.with(|f| f.beam_deg.clone()))
                set=Box::new(move |r| filter.update(|f| f.beam_deg = r))
            />
            <CriMin filter/>
        </section>
        <section class="ranges">
            <h4>"Mounting"</h4>
            <DualSliderU32
                label="Recessed depth (mm)"
                step=1
                bounds=depth_b
                get=Box::new(move || filter.with(|f| f.recessed_depth_mm.clone()))
                set=Box::new(move |r| filter.update(|f| f.recessed_depth_mm = r))
            />
        </section>
    }
}

/// Dual-handle slider for an `f32` range axis, backed by dynamic
/// rails from a `Resource<RangeBounds>`. Renders:
/// - a `(min – max)` badge with the live value pair,
/// - two overlaid `<input type="range">` (the dual-handle trick),
/// - a `<details>` containing the existing `RangeF32` boxes for
///   precise typing.
///
/// When `bounds` is `None` (no data for the current filter on this
/// axis) the slider renders disabled with a tooltip.
#[component]
fn DualSliderF32(
    label: &'static str,
    step: f64,
    bounds: Memo<Option<F32Bounds>>,
    get: Box<dyn Fn() -> Option<core::ops::RangeInclusive<f32>> + Send + Sync + 'static>,
    set: Box<dyn Fn(Option<core::ops::RangeInclusive<f32>>) + Send + Sync + 'static>,
) -> impl IntoView {
    let get = std::sync::Arc::new(get);
    let set = std::sync::Arc::new(set);

    // Default rails until the resource lands. We use 0..0 so the
    // slider thumbs don't drift while loading; the CSS marks the
    // row as disabled on `bounds.is_none()`.
    let rail_lo = Memo::new(move |_| bounds.get().map(|b| b.min).unwrap_or(0.0));
    let rail_hi = Memo::new(move |_| bounds.get().map(|b| b.max).unwrap_or(0.0));

    // Current min/max handle positions. If no filter is set, the
    // handles sit at the rails (no narrowing yet). Otherwise the
    // saved filter wins.
    let handle_min = {
        let get = get.clone();
        Memo::new(move |_| match get() {
            Some(r) => *r.start(),
            None => rail_lo.get(),
        })
    };
    let handle_max = {
        let get = get.clone();
        Memo::new(move |_| match get() {
            Some(r) => *r.end(),
            None => rail_hi.get(),
        })
    };

    let on_min = {
        let set = set.clone();
        move |ev: leptos::ev::Event| {
            let raw = event_target_value(&ev);
            let Ok(parsed) = raw.parse::<f32>() else {
                return;
            };
            // Echo guard: when `prop:value` writes a new value to the
            // <input>, the browser may fire an `input` event that
            // arrives back here. If the event value matches what the
            // handle would already report, this is an echo from our
            // own programmatic write — ignore it. Without this guard,
            // a filter reset triggers a rails update → handle Memo
            // update → DOM value write → `input` event → on_min →
            // set(Some(...)) → filter change → infinite refresh loop.
            // The reset never lands and "Searching…" sticks.
            let current = handle_min.get_untracked();
            if (parsed - current).abs() < f32::EPSILON {
                return;
            }
            let mut min_v = parsed;
            let max_v = handle_max.get_untracked();
            if min_v > max_v {
                min_v = max_v;
            }
            // If both ends now sit at the rails, treat as no filter.
            let (lo, hi) = (rail_lo.get_untracked(), rail_hi.get_untracked());
            if (min_v - lo).abs() < f32::EPSILON && (max_v - hi).abs() < f32::EPSILON {
                set(None);
            } else {
                set(Some(min_v..=max_v));
            }
        }
    };
    let on_max = {
        let set = set.clone();
        move |ev: leptos::ev::Event| {
            let raw = event_target_value(&ev);
            let Ok(parsed) = raw.parse::<f32>() else {
                return;
            };
            let current = handle_max.get_untracked();
            if (parsed - current).abs() < f32::EPSILON {
                return;
            }
            let mut max_v = parsed;
            let min_v = handle_min.get_untracked();
            if max_v < min_v {
                max_v = min_v;
            }
            let (lo, hi) = (rail_lo.get_untracked(), rail_hi.get_untracked());
            if (min_v - lo).abs() < f32::EPSILON && (max_v - hi).abs() < f32::EPSILON {
                set(None);
            } else {
                set(Some(min_v..=max_v));
            }
        }
    };

    let disabled = Memo::new(move |_| bounds.get().is_none());
    let tooltip = move || {
        if disabled.get() {
            Some("no data for current filters")
        } else {
            None
        }
    };

    let badge = move || {
        if disabled.get() {
            "—".to_string()
        } else {
            format!(
                "{} – {}",
                format_f32(handle_min.get()),
                format_f32(handle_max.get())
            )
        }
    };

    view! {
        <div class="range-row" class:range-disabled=move || disabled.get()>
            <div class="range-head">
                <span class="range-label">{label}</span>
                <span class="range-badge">{badge}</span>
            </div>
            <div class="dual-slider" title=tooltip>
                <input
                    type="range"
                    class="dual-min"
                    disabled=move || disabled.get()
                    min=move || rail_lo.get() as f64
                    max=move || rail_hi.get() as f64
                    step=step
                    prop:value=move || handle_min.get() as f64
                    on:input=on_min
                />
                <input
                    type="range"
                    class="dual-max"
                    disabled=move || disabled.get()
                    min=move || rail_lo.get() as f64
                    max=move || rail_hi.get() as f64
                    step=step
                    prop:value=move || handle_max.get() as f64
                    on:input=on_max
                />
            </div>
            <details class="range-exact">
                <summary>"enter exact value"</summary>
                <RangeF32
                    label=""
                    get=Box::new({
                        let get = get.clone();
                        move || get()
                    })
                    set=Box::new({
                        let set = set.clone();
                        move |r| set(r)
                    })
                />
            </details>
        </div>
    }
}

/// Dual-handle slider for a `u16` range axis (CCT). Same shape as
/// [`DualSliderF32`].
#[component]
fn DualSliderU16(
    label: &'static str,
    step: u16,
    bounds: Memo<Option<U16Bounds>>,
    get: Box<dyn Fn() -> Option<core::ops::RangeInclusive<u16>> + Send + Sync + 'static>,
    set: Box<dyn Fn(Option<core::ops::RangeInclusive<u16>>) + Send + Sync + 'static>,
) -> impl IntoView {
    let get = std::sync::Arc::new(get);
    let set = std::sync::Arc::new(set);

    let rail_lo = Memo::new(move |_| bounds.get().map(|b| b.min).unwrap_or(0));
    let rail_hi = Memo::new(move |_| bounds.get().map(|b| b.max).unwrap_or(0));

    let handle_min = {
        let get = get.clone();
        Memo::new(move |_| match get() {
            Some(r) => *r.start(),
            None => rail_lo.get(),
        })
    };
    let handle_max = {
        let get = get.clone();
        Memo::new(move |_| match get() {
            Some(r) => *r.end(),
            None => rail_hi.get(),
        })
    };

    let on_min = {
        let set = set.clone();
        move |ev: leptos::ev::Event| {
            let Ok(parsed) = event_target_value(&ev).parse::<u16>() else {
                return;
            };
            // Echo guard — see `DualSliderF32` for the rationale.
            if parsed == handle_min.get_untracked() {
                return;
            }
            let mut min_v = parsed;
            let max_v = handle_max.get_untracked();
            if min_v > max_v {
                min_v = max_v;
            }
            let (lo, hi) = (rail_lo.get_untracked(), rail_hi.get_untracked());
            if min_v == lo && max_v == hi {
                set(None);
            } else {
                set(Some(min_v..=max_v));
            }
        }
    };
    let on_max = {
        let set = set.clone();
        move |ev: leptos::ev::Event| {
            let Ok(parsed) = event_target_value(&ev).parse::<u16>() else {
                return;
            };
            if parsed == handle_max.get_untracked() {
                return;
            }
            let mut max_v = parsed;
            let min_v = handle_min.get_untracked();
            if max_v < min_v {
                max_v = min_v;
            }
            let (lo, hi) = (rail_lo.get_untracked(), rail_hi.get_untracked());
            if min_v == lo && max_v == hi {
                set(None);
            } else {
                set(Some(min_v..=max_v));
            }
        }
    };

    let disabled = Memo::new(move |_| bounds.get().is_none());
    let badge = move || {
        if disabled.get() {
            "—".to_string()
        } else {
            format!("{} – {}", handle_min.get(), handle_max.get())
        }
    };

    view! {
        <div class="range-row" class:range-disabled=move || disabled.get()>
            <div class="range-head">
                <span class="range-label">{label}</span>
                <span class="range-badge">{badge}</span>
            </div>
            <div class="dual-slider">
                <input
                    type="range"
                    class="dual-min"
                    disabled=move || disabled.get()
                    min=move || rail_lo.get() as f64
                    max=move || rail_hi.get() as f64
                    step=step
                    prop:value=move || handle_min.get() as f64
                    on:input=on_min
                />
                <input
                    type="range"
                    class="dual-max"
                    disabled=move || disabled.get()
                    min=move || rail_lo.get() as f64
                    max=move || rail_hi.get() as f64
                    step=step
                    prop:value=move || handle_max.get() as f64
                    on:input=on_max
                />
            </div>
            <details class="range-exact">
                <summary>"enter exact value"</summary>
                <RangeU16
                    label=""
                    get=Box::new({
                        let get = get.clone();
                        move || get()
                    })
                    set=Box::new({
                        let set = set.clone();
                        move |r| set(r)
                    })
                />
            </details>
        </div>
    }
}

/// Dual-handle slider for a `u32` range axis (recessed depth). Same
/// shape as [`DualSliderF32`].
#[component]
fn DualSliderU32(
    label: &'static str,
    step: u32,
    bounds: Memo<Option<U32Bounds>>,
    get: Box<dyn Fn() -> Option<core::ops::RangeInclusive<u32>> + Send + Sync + 'static>,
    set: Box<dyn Fn(Option<core::ops::RangeInclusive<u32>>) + Send + Sync + 'static>,
) -> impl IntoView {
    let get = std::sync::Arc::new(get);
    let set = std::sync::Arc::new(set);

    let rail_lo = Memo::new(move |_| bounds.get().map(|b| b.min).unwrap_or(0));
    let rail_hi = Memo::new(move |_| bounds.get().map(|b| b.max).unwrap_or(0));

    let handle_min = {
        let get = get.clone();
        Memo::new(move |_| match get() {
            Some(r) => *r.start(),
            None => rail_lo.get(),
        })
    };
    let handle_max = {
        let get = get.clone();
        Memo::new(move |_| match get() {
            Some(r) => *r.end(),
            None => rail_hi.get(),
        })
    };

    let on_min = {
        let set = set.clone();
        move |ev: leptos::ev::Event| {
            let Ok(parsed) = event_target_value(&ev).parse::<u32>() else {
                return;
            };
            // Echo guard — see `DualSliderF32` for the rationale.
            if parsed == handle_min.get_untracked() {
                return;
            }
            let mut min_v = parsed;
            let max_v = handle_max.get_untracked();
            if min_v > max_v {
                min_v = max_v;
            }
            let (lo, hi) = (rail_lo.get_untracked(), rail_hi.get_untracked());
            if min_v == lo && max_v == hi {
                set(None);
            } else {
                set(Some(min_v..=max_v));
            }
        }
    };
    let on_max = {
        let set = set.clone();
        move |ev: leptos::ev::Event| {
            let Ok(parsed) = event_target_value(&ev).parse::<u32>() else {
                return;
            };
            if parsed == handle_max.get_untracked() {
                return;
            }
            let mut max_v = parsed;
            let min_v = handle_min.get_untracked();
            if max_v < min_v {
                max_v = min_v;
            }
            let (lo, hi) = (rail_lo.get_untracked(), rail_hi.get_untracked());
            if min_v == lo && max_v == hi {
                set(None);
            } else {
                set(Some(min_v..=max_v));
            }
        }
    };

    let disabled = Memo::new(move |_| bounds.get().is_none());
    let badge = move || {
        if disabled.get() {
            "—".to_string()
        } else {
            format!("{} – {}", handle_min.get(), handle_max.get())
        }
    };

    view! {
        <div class="range-row" class:range-disabled=move || disabled.get()>
            <div class="range-head">
                <span class="range-label">{label}</span>
                <span class="range-badge">{badge}</span>
            </div>
            <div class="dual-slider">
                <input
                    type="range"
                    class="dual-min"
                    disabled=move || disabled.get()
                    min=move || rail_lo.get() as f64
                    max=move || rail_hi.get() as f64
                    step=step
                    prop:value=move || handle_min.get() as f64
                    on:input=on_min
                />
                <input
                    type="range"
                    class="dual-max"
                    disabled=move || disabled.get()
                    min=move || rail_lo.get() as f64
                    max=move || rail_hi.get() as f64
                    step=step
                    prop:value=move || handle_max.get() as f64
                    on:input=on_max
                />
            </div>
            <details class="range-exact">
                <summary>"enter exact value"</summary>
                <RangeU32
                    label=""
                    get=Box::new({
                        let get = get.clone();
                        move || get()
                    })
                    set=Box::new({
                        let set = set.clone();
                        move |r| set(r)
                    })
                />
            </details>
        </div>
    }
}

/// Format an `f32` for the slider's read-only badge. Drops trailing
/// zeros so `1500.0` renders as `1500` but `0.5` stays as `0.5`.
fn format_f32(v: f32) -> String {
    if (v - v.round()).abs() < 1e-3 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

/// Paired (min, max) `f32` range input. Each side is independent —
/// blanking one keeps the other.
#[component]
fn RangeF32(
    label: &'static str,
    get: Box<dyn Fn() -> Option<core::ops::RangeInclusive<f32>> + Send + Sync + 'static>,
    set: Box<dyn Fn(Option<core::ops::RangeInclusive<f32>>) + Send + Sync + 'static>,
) -> impl IntoView {
    let get = std::sync::Arc::new(get);
    let set = std::sync::Arc::new(set);

    let get_min = {
        let get = get.clone();
        move || get().map(|r| format!("{}", r.start())).unwrap_or_default()
    };
    let get_max = {
        let get = get.clone();
        move || get().map(|r| format!("{}", r.end())).unwrap_or_default()
    };

    let on_min = {
        let get = get.clone();
        let set = set.clone();
        move |ev: leptos::ev::Event| {
            let raw = event_target_value(&ev);
            let cur = get();
            let max = cur.as_ref().map(|r| *r.end()).unwrap_or(f32::INFINITY);
            let new_min = raw.trim().parse::<f32>().ok();
            set(build_range_f32(new_min, finite_or_none(max)));
        }
    };
    let on_max = {
        let get = get.clone();
        let set = set.clone();
        move |ev: leptos::ev::Event| {
            let raw = event_target_value(&ev);
            let cur = get();
            let min = cur
                .as_ref()
                .map(|r| *r.start())
                .unwrap_or(f32::NEG_INFINITY);
            let new_max = raw.trim().parse::<f32>().ok();
            set(build_range_f32(finite_or_none(min), new_max));
        }
    };

    view! {
        <label class="range-row">
            <span class="range-label">{label}</span>
            <span class="range-inputs">
                <input
                    type="number"
                    class="range-input"
                    placeholder="min"
                    prop:value=get_min
                    on:input=on_min
                />
                <span class="range-dash">"–"</span>
                <input
                    type="number"
                    class="range-input"
                    placeholder="max"
                    prop:value=get_max
                    on:input=on_max
                />
            </span>
        </label>
    }
}

/// Paired (min, max) `u16` range input. Used for CCT_K.
#[component]
fn RangeU16(
    label: &'static str,
    get: Box<dyn Fn() -> Option<core::ops::RangeInclusive<u16>> + Send + Sync + 'static>,
    set: Box<dyn Fn(Option<core::ops::RangeInclusive<u16>>) + Send + Sync + 'static>,
) -> impl IntoView {
    let get = std::sync::Arc::new(get);
    let set = std::sync::Arc::new(set);

    let get_min = {
        let get = get.clone();
        move || get().map(|r| format!("{}", r.start())).unwrap_or_default()
    };
    let get_max = {
        let get = get.clone();
        move || get().map(|r| format!("{}", r.end())).unwrap_or_default()
    };

    let on_min = {
        let get = get.clone();
        let set = set.clone();
        move |ev: leptos::ev::Event| {
            let raw = event_target_value(&ev);
            let cur = get();
            let max = cur.as_ref().map(|r| *r.end());
            let new_min = raw.trim().parse::<u16>().ok();
            set(build_range_u16(new_min, max));
        }
    };
    let on_max = {
        let get = get.clone();
        let set = set.clone();
        move |ev: leptos::ev::Event| {
            let raw = event_target_value(&ev);
            let cur = get();
            let min = cur.as_ref().map(|r| *r.start());
            let new_max = raw.trim().parse::<u16>().ok();
            set(build_range_u16(min, new_max));
        }
    };

    view! {
        <label class="range-row">
            <span class="range-label">{label}</span>
            <span class="range-inputs">
                <input
                    type="number"
                    class="range-input"
                    placeholder="min"
                    prop:value=get_min
                    on:input=on_min
                />
                <span class="range-dash">"–"</span>
                <input
                    type="number"
                    class="range-input"
                    placeholder="max"
                    prop:value=get_max
                    on:input=on_max
                />
            </span>
        </label>
    }
}

/// Paired (min, max) `u32` range input. Used for recessed depth (mm).
#[component]
fn RangeU32(
    label: &'static str,
    get: Box<dyn Fn() -> Option<core::ops::RangeInclusive<u32>> + Send + Sync + 'static>,
    set: Box<dyn Fn(Option<core::ops::RangeInclusive<u32>>) + Send + Sync + 'static>,
) -> impl IntoView {
    let get = std::sync::Arc::new(get);
    let set = std::sync::Arc::new(set);

    let get_min = {
        let get = get.clone();
        move || get().map(|r| format!("{}", r.start())).unwrap_or_default()
    };
    let get_max = {
        let get = get.clone();
        move || get().map(|r| format!("{}", r.end())).unwrap_or_default()
    };

    let on_min = {
        let get = get.clone();
        let set = set.clone();
        move |ev: leptos::ev::Event| {
            let raw = event_target_value(&ev);
            let cur = get();
            let max = cur.as_ref().map(|r| *r.end());
            let new_min = raw.trim().parse::<u32>().ok();
            set(build_range_u32(new_min, max));
        }
    };
    let on_max = {
        let get = get.clone();
        let set = set.clone();
        move |ev: leptos::ev::Event| {
            let raw = event_target_value(&ev);
            let cur = get();
            let min = cur.as_ref().map(|r| *r.start());
            let new_max = raw.trim().parse::<u32>().ok();
            set(build_range_u32(min, new_max));
        }
    };

    view! {
        <label class="range-row">
            <span class="range-label">{label}</span>
            <span class="range-inputs">
                <input
                    type="number"
                    class="range-input"
                    placeholder="min"
                    prop:value=get_min
                    on:input=on_min
                />
                <span class="range-dash">"–"</span>
                <input
                    type="number"
                    class="range-input"
                    placeholder="max"
                    prop:value=get_max
                    on:input=on_max
                />
            </span>
        </label>
    }
}

/// Single-threshold CRI minimum. Variants below this Ra value are
/// filtered out; blank clears.
#[component]
fn CriMin(filter: RwSignal<Filter>) -> impl IntoView {
    let get_val = move || filter.with(|f| f.cri_min.map(|v| v.to_string()).unwrap_or_default());
    let on_input = move |ev: leptos::ev::Event| {
        let raw = event_target_value(&ev);
        let parsed = raw.trim().parse::<u8>().ok();
        filter.update(|f| f.cri_min = parsed);
    };

    view! {
        <label class="range-row">
            <span class="range-label">"CRI Ra ≥"</span>
            <span class="range-inputs">
                <input
                    type="number"
                    class="range-input range-single"
                    min="0"
                    max="100"
                    placeholder="min"
                    prop:value=get_val
                    on:input=on_input
                />
            </span>
        </label>
    }
}

fn finite_or_none(v: f32) -> Option<f32> {
    if v.is_finite() {
        Some(v)
    } else {
        None
    }
}

fn build_range_f32(min: Option<f32>, max: Option<f32>) -> Option<core::ops::RangeInclusive<f32>> {
    match (min, max) {
        (None, None) => None,
        (Some(lo), Some(hi)) => Some(lo..=hi),
        // One-sided: the schema's range type is `RangeInclusive` so
        // "unbounded on that side" maps to ±∞. `matches_variant` uses
        // an open-coded comparison that handles infinities correctly.
        (Some(lo), None) => Some(lo..=f32::INFINITY),
        (None, Some(hi)) => Some(f32::NEG_INFINITY..=hi),
    }
}

fn build_range_u16(min: Option<u16>, max: Option<u16>) -> Option<core::ops::RangeInclusive<u16>> {
    match (min, max) {
        (None, None) => None,
        (Some(lo), Some(hi)) => Some(lo..=hi),
        (Some(lo), None) => Some(lo..=u16::MAX),
        (None, Some(hi)) => Some(0..=hi),
    }
}

fn build_range_u32(min: Option<u32>, max: Option<u32>) -> Option<core::ops::RangeInclusive<u32>> {
    match (min, max) {
        (None, None) => None,
        (Some(lo), Some(hi)) => Some(lo..=hi),
        (Some(lo), None) => Some(lo..=u32::MAX),
        (None, Some(hi)) => Some(0..=hi),
    }
}

/// One column inside the facet panel. Generic over the per-value
/// "is this selected?" predicate and toggle callback so the parent
/// can wire each facet's clicks to the right slot of the [`Filter`].
///
/// `glyph` is an optional lookup: when present, the function is
/// called per value and the returned SVG markup (if any) is inlined
/// before the label. Used for the Light-distribution group, where
/// each value has a synthesized polar-curve glyph from
/// [`crate::glyphs::lvk_glyph`].
/// Hierarchical facet panel for the 79-entry Application taxonomy.
///
/// XSD strings look like `"Interior: Office: Desk"` — three colon-
/// separated segments. We group rows by top (`Interior`/`Exterior`),
/// then by mid (`Office`, `Retail`, ...). Two-segment entries like
/// `"Exterior: Other"` render as a leaf directly under their top.
///
/// Each level is a `<details>` so the tree is keyboard- and
/// screen-reader-accessible with zero JS. The top groups stay open by
/// default; mid groups stay closed to keep the panel short.
#[component]
fn ApplicationTree(values: Vec<(String, u32)>, filter: RwSignal<Filter>) -> impl IntoView {
    if values.is_empty() {
        // Render the empty section anyway so the user knows the facet
        // exists — the previous flat FacetGroup silently disappeared,
        // and the user reported "no Applications in the frontend".
        // Most likely cause: corpus is sparse on
        // `DescriptiveAttributes/Marketing/Applications` (the
        // 270k-doc light-other-rs export carries application metadata
        // only on a small subset). Make the absence visible instead
        // of invisible.
        return view! {
            <section class="facet-group facet-tree">
                <h4>
                    <span class="facet-group-name">"Application"</span>
                    <span class="facet-group-count">0</span>
                </h4>
                <p class="facet-empty">"No applications declared in this slice."</p>
            </section>
        }
        .into_any();
    }

    // Bucket by (top, mid) where mid is "" for the two-segment "Top: Leaf"
    // shape. Preserve XSD order within each bucket by using BTreeMap on a
    // (top_first_seen_idx, mid_first_seen_idx) key — but a simpler
    // approach: walk once, append. The canonical XSD list is already in
    // a sensible order, and `count_one_facet` emits in that order.
    use std::collections::BTreeMap;

    // Order-preserving bucketing. `Vec<(top, Vec<(mid, Vec<row>)>)>`.
    type Row = (String, u32); // (canonical XSD string, count)
    let mut tops: Vec<(&'static str, Vec<(&'static str, Vec<Row>)>)> = Vec::new();

    // Helper closures to insert at end-of-list preserving first-seen order.
    fn upsert_top<'a>(
        tops: &'a mut Vec<(&'static str, Vec<(&'static str, Vec<Row>)>)>,
        top: &'static str,
    ) -> &'a mut Vec<(&'static str, Vec<Row>)> {
        if let Some(i) = tops.iter().position(|(t, _)| *t == top) {
            return &mut tops[i].1;
        }
        tops.push((top, Vec::new()));
        &mut tops.last_mut().unwrap().1
    }
    fn upsert_mid<'a>(
        mids: &'a mut Vec<(&'static str, Vec<Row>)>,
        mid: &'static str,
    ) -> &'a mut Vec<Row> {
        if let Some(i) = mids.iter().position(|(m, _)| *m == mid) {
            return &mut mids[i].1;
        }
        mids.push((mid, Vec::new()));
        &mut mids.last_mut().unwrap().1
    }

    // Track total rows + bucket counts for the badge.
    let mut total_rows = 0u32;
    let mut top_count: BTreeMap<&'static str, u32> = BTreeMap::new();
    let mut mid_count: BTreeMap<(&'static str, &'static str), u32> = BTreeMap::new();

    for (value, count) in values {
        let Some(id) = application_from_str(&value) else {
            continue;
        };
        let top = application_top(id).unwrap_or("Other");
        let mid = application_mid(id).unwrap_or("");
        total_rows += 1;
        *top_count.entry(top).or_default() += count;
        if !mid.is_empty() {
            *mid_count.entry((top, mid)).or_default() += count;
        }
        let mids = upsert_top(&mut tops, top);
        let leaves = upsert_mid(mids, mid);
        leaves.push((value, count));
    }

    let overflows = total_rows > 6;
    let total_label = total_rows as usize;

    view! {
        <section class="facet-group facet-tree" class:facet-group-overflows=move || overflows>
            <h4>
                <span class="facet-group-name">"Application"</span>
                <span class="facet-group-count">{total_label}</span>
            </h4>
            <ul class="facet-tree-roots">
                {tops.into_iter().map(|(top, mids)| {
                    let top_total = top_count.get(top).copied().unwrap_or(0);
                    view! {
                        <li class="facet-tree-top">
                            <details open=true>
                                <summary>
                                    <span class="facet-tree-top-label">{top}</span>
                                    <span class="facet-count">{top_total}</span>
                                </summary>
                                <ul class="facet-tree-mids">
                                    {mids.into_iter().map(|(mid, leaves)| {
                                        application_tree_branch(mid, leaves, top, mid_count.get(&(top, mid)).copied().unwrap_or(0), filter)
                                    }).collect_view()}
                                </ul>
                            </details>
                        </li>
                    }
                }).collect_view()}
            </ul>
        </section>
    }
    .into_any()
}

/// Render one mid-branch of the Application tree. When `mid` is empty
/// (the two-segment "Top: Leaf" case) we drop the wrapping `<details>`
/// and render the leaves directly under the top group.
fn application_tree_branch(
    mid: &'static str,
    leaves: Vec<(String, u32)>,
    top: &'static str,
    mid_total: u32,
    filter: RwSignal<Filter>,
) -> impl IntoView {
    let rows = leaves
        .into_iter()
        .map(|(value, count)| application_leaf_row(value, count, top, mid, filter))
        .collect_view();

    if mid.is_empty() {
        view! { <>{rows}</> }.into_any()
    } else {
        view! {
            <li class="facet-tree-mid">
                <details>
                    <summary>
                        <span class="facet-tree-mid-label">{mid}</span>
                        <span class="facet-count">{mid_total}</span>
                    </summary>
                    <ul class="facet-tree-leaves">{rows}</ul>
                </details>
            </li>
        }
        .into_any()
    }
}

/// One selectable leaf row in the Application tree. Reuses the same
/// `.facet-value` button class as `FacetGroup` so styling stays in
/// lockstep — only the label is "Top → Mid → Leaf"-derived rather than
/// the full XSD string.
fn application_leaf_row(
    value: String,
    count: u32,
    _top: &'static str,
    _mid: &'static str,
    filter: RwSignal<Filter>,
) -> impl IntoView {
    let Some(id) = application_from_str(&value) else {
        return ().into_any();
    };
    // Leaf = last `:`-separated segment of the canonical XSD string.
    // Falls back to the full value if there's no colon (shouldn't
    // happen for canonical applications, but keep it robust).
    let leaf_label = value
        .rsplit(':')
        .next()
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| value.clone());

    let value_for_click = value;
    view! {
        <li>
            <button
                class="facet-value"
                class:facet-selected=move || filter.with(|f| f.applications.contains(&id))
                on:click=move |_| {
                    let _ = &value_for_click;
                    filter.update(|f| toggle_id(&mut f.applications, id));
                }
            >
                <span class="facet-label">{leaf_label}</span>
                <span class="facet-count">{count}</span>
            </button>
        </li>
    }
    .into_any()
}

#[component]
fn FacetGroup(
    name: &'static str,
    values: Vec<(String, u32)>,
    is_selected: Box<dyn Fn(&str) -> bool + Send + Sync + 'static>,
    on_toggle: Box<dyn Fn(&str) + Send + Sync + 'static>,
    #[prop(optional)] glyph: Option<fn(&str) -> Option<&'static str>>,
) -> impl IntoView {
    if values.is_empty() {
        return ().into_any();
    }
    let total = values.len();
    let is_selected = std::sync::Arc::new(is_selected);
    let on_toggle = std::sync::Arc::new(on_toggle);

    // Hint for the CSS: when the list has more than ~6 rows it
    // overflows the 12 rem cap and we want the bottom-fade affordance
    // to tell the user. Threshold matches the rendered row height so
    // single-screen lists don't get a fake "more below" cue.
    let overflows = total > 6;

    view! {
        <section class="facet-group" class:facet-group-overflows=move || overflows>
            <h4>
                <span class="facet-group-name">{name}</span>
                <span class="facet-group-count">{total}</span>
            </h4>
            <ul>
                {values.into_iter().map(|(value, count)| {
                    let is_sel = is_selected.clone();
                    let toggle = on_toggle.clone();
                    let value_for_check = value.clone();
                    let value_for_click = value.clone();
                    let value_for_label = value.clone();
                    // Resolve the glyph at render time. Static `&str`
                    // means we can pass it straight into `inner_html`
                    // without an allocation.
                    let svg = glyph.and_then(|g| g(&value));
                    let glyph_slot = svg.map(|markup| {
                        view! {
                            <span class="facet-glyph" inner_html=markup></span>
                        }
                    });
                    view! {
                        <li>
                            <button
                                class="facet-value"
                                class:facet-with-glyph=move || svg.is_some()
                                class:facet-selected=move || is_sel(&value_for_check)
                                on:click=move |_| toggle(&value_for_click)
                            >
                                {glyph_slot}
                                <span class="facet-label">{value_for_label}</span>
                                <span class="facet-count">{count}</span>
                            </button>
                        </li>
                    }
                }).collect_view()}
            </ul>
        </section>
    }
    .into_any()
}

// ── Filter mutation helpers ───────────────────────────────────────────

fn toggle_id<const N: usize>(slot: &mut SmallVec<[u8; N]>, id: u8) {
    if let Some(pos) = slot.iter().position(|x| *x == id) {
        slot.swap_remove(pos);
    } else {
        slot.push(id);
    }
}

/// Reverse `format!("{:?}", MountingPlace)` — same form the index
/// emits in `emit_doc_values`. Keep these in lockstep.
fn mounting_place_from_str(s: &str) -> Option<MountingPlace> {
    Some(match s {
        "Ceiling" => MountingPlace::Ceiling,
        "Wall" => MountingPlace::Wall,
        "WorkingPlane" => MountingPlace::WorkingPlane,
        "Ground" => MountingPlace::Ground,
        "Unknown" => MountingPlace::Unknown,
        _ => return None,
    })
}

/// Reverse `format!("{:?}", MountingType)`.
fn mounting_type_from_str(s: &str) -> Option<MountingType> {
    Some(match s {
        "Recessed" => MountingType::Recessed,
        "SurfaceMounted" => MountingType::SurfaceMounted,
        "Pendant" => MountingType::Pendant,
        "FreeStanding" => MountingType::FreeStanding,
        "PoleTop" => MountingType::PoleTop,
        "PoleIntegrated" => MountingType::PoleIntegrated,
        "Unknown" => MountingType::Unknown,
        _ => return None,
    })
}

/// Toggle `g` in `slot`: remove on hit, push on miss. Generic over
/// any PartialEq+Copy value (used by both MountingPlace and
/// MountingType).
fn toggle_in_vec<T: PartialEq + Copy, const N: usize>(slot: &mut SmallVec<[T; N]>, g: T) {
    if let Some(pos) = slot.iter().position(|x| *x == g) {
        slot.swap_remove(pos);
    } else {
        slot.push(g);
    }
}

fn toggle_manufacturer(f: &mut Filter, name: &str) {
    let cs = CompactString::from(name);
    match &mut f.manufacturer {
        Some(list) => {
            if let Some(pos) = list.iter().position(|m| *m == cs) {
                list.swap_remove(pos);
                if list.is_empty() {
                    f.manufacturer = None;
                }
            } else {
                list.push(cs);
            }
        }
        None => {
            let mut list = SmallVec::new();
            list.push(cs);
            f.manufacturer = Some(list);
        }
    }
}

/// Variant-grain row: one row per matching variant. After group-
/// and-fold, the in-RAM doc store carries family rollups; each
/// search hit picks out a specific variant inside its family. The
/// row shows the family header (manufacturer, product) plus the
/// variant's specific photometry, with a polar+image expand that
/// loads THIS variant's LDT and product picture.
///
/// Replaces the doc-grain `HitRow` in the result list. The variant
/// drilldown table that `HitRow` exposed is no longer needed at this
/// level — each row already IS a variant.
#[component]
pub fn VariantRow(payload: crate::api::VariantPayloadSlim) -> impl IntoView {
    let crate::api::VariantPayloadSlim {
        doc_id_hex: id_hex,
        variant_index: _variant_index,
        manufacturer,
        product,
        variant_name,
        sku,
        viewer_url,
        phot,
        ip_code,
        lamp_type,
        control_gear_interfaces,
        applications,
        mounting,
        recessed_depth_mm,
        has_3d,
        has_emergency_lighting,
        has_photometry: has_phot,
        description_snippet,
        description_truncated,
    } = payload;
    // wasm-only: the spawn_local closures need variant_index by
    // value. SSR doesn't compile those, so we destructure with
    // an underscore to silence the unused-binding warning, then
    // bind a local for the wasm cfg.
    #[cfg(target_arch = "wasm32")]
    let variant_index = _variant_index;

    let stats = phot.as_ref().map(render_phot_stats_slim);

    let polar_open = RwSignal::new(false);
    let polar_svg: RwSignal<Option<String>> = RwSignal::new(None);
    let polar_loading = RwSignal::new(false);
    let product_image: RwSignal<Option<Option<crate::api::ProductImage>>> = RwSignal::new(None);
    let image_loading = RwSignal::new(false);

    let toggle_polar = {
        let id_hex = id_hex.clone();
        move |_| {
            if polar_open.get() {
                polar_open.set(false);
                return;
            }
            polar_open.set(true);
            polar_svg.set(None);
            product_image.set(None);
            polar_loading.set(true);
            image_loading.set(true);
            let id_hex_p = id_hex.clone();
            let id_hex_i = id_hex.clone();
            #[cfg(target_arch = "wasm32")]
            {
                leptos::task::spawn_local(async move {
                    let res =
                        crate::api::fetch_ldt_for_variant(id_hex_p, Some(variant_index)).await;
                    polar_loading.set(false);
                    match res {
                        Ok(Some(ldt_text)) => {
                            let svg = crate::polar_client::render_polar_from_ldt(&ldt_text, 240.0)
                                .unwrap_or_default();
                            polar_svg.set(Some(svg));
                        }
                        Ok(None) => polar_svg.set(Some(String::new())),
                        Err(_) => polar_svg.set(Some(String::new())),
                    }
                });
                leptos::task::spawn_local(async move {
                    let res =
                        crate::api::fetch_product_image_for_variant(id_hex_i, Some(variant_index))
                            .await;
                    image_loading.set(false);
                    match res {
                        Ok(payload) => product_image.set(Some(payload)),
                        Err(_) => product_image.set(Some(None)),
                    }
                });
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let _ = (id_hex_p, id_hex_i);
                polar_loading.set(false);
                image_loading.set(false);
            }
        }
    };

    let polar_button_enabled = has_phot;
    let description_open = RwSignal::new(false);
    let toggle_description = move |_| description_open.update(|v| *v = !*v);

    // Build the chips row contents. Each chip is a small inline
    // badge; we render IP / lamp / control gear / mounting / depth
    // / 3D / emergency / applications. Empty chips lists collapse.
    let cg_chips: Vec<String> = control_gear_interfaces;
    let app_chips: Vec<String> = applications;
    let has_chips = ip_code.is_some()
        || lamp_type.is_some()
        || !cg_chips.is_empty()
        || mounting.is_some()
        || recessed_depth_mm.is_some()
        || has_3d
        || has_emergency_lighting
        || !app_chips.is_empty();

    let desc_for_full = description_snippet.clone();

    view! {
        <li class="hit">
            <div class="hit-header">
                <span class="hit-manufacturer">{manufacturer}</span>
                <span class="hit-product">{product}</span>
                <span class="hit-variant-name">{variant_name}</span>
                {(!has_phot).then(|| view! {
                    <span class="badge badge-missing">"no photometry"</span>
                })}
                {polar_button_enabled.then(|| view! {
                    <button
                        class="polar-toggle"
                        title="Show polar curve + product image"
                        on:click=toggle_polar
                    >
                        {move || if polar_open.get() { "↑ Hide polar" } else { "↓ Polar" }}
                    </button>
                })}
                {viewer_url.map(|href| view! {
                    <a class="viewer-link" href=href target="_blank" rel="noopener">
                        "Open in viewer ↗"
                    </a>
                })}
            </div>
            {sku.map(|code| view! {
                <div class="hit-sku">{code}</div>
            })}
            {stats}
            {has_chips.then(|| view! {
                <div class="hit-chips">
                    {ip_code.map(|s| view! { <span class="hit-chip hit-chip-ip">{s}</span> })}
                    {lamp_type.map(|s| view! { <span class="hit-chip hit-chip-lamp">{s}</span> })}
                    {(!cg_chips.is_empty()).then(|| view! {
                        <>{cg_chips.into_iter().map(|s| view! {
                            <span class="hit-chip hit-chip-cg">{s}</span>
                        }).collect_view()}</>
                    })}
                    {mounting.map(|s| view! { <span class="hit-chip hit-chip-mount">{s}</span> })}
                    {recessed_depth_mm.map(|d| view! {
                        <span class="hit-chip hit-chip-depth">{format!("{d} mm")}</span>
                    })}
                    {has_3d.then(|| view! { <span class="hit-chip hit-chip-3d">"3D"</span> })}
                    {has_emergency_lighting.then(|| view! {
                        <span class="hit-chip hit-chip-em">"emergency"</span>
                    })}
                    {(!app_chips.is_empty()).then(|| view! {
                        <>{app_chips.into_iter().map(|s| {
                            // Drop the "Interior:" / "Exterior:"
                            // top-level prefix in the chip — saves
                            // horizontal space and the top level is
                            // visually carried by the leading colon-
                            // separated segment.
                            let label = s
                                .rsplit(':')
                                .next()
                                .map(|t| t.trim().to_string())
                                .unwrap_or_else(|| s.clone());
                            view! { <span class="hit-chip hit-chip-app">{label}</span> }
                        }).collect_view()}</>
                    })}
                </div>
            })}
            {description_snippet.map(|snippet| view! {
                <div class="hit-description">
                    <p class="hit-description-text">
                        {move || if description_open.get() {
                            // Full description requires a separate
                            // fetch (snippet is what we shipped).
                            // For now we expand to the snippet
                            // itself; a future fetch_description
                            // can supply the un-truncated text.
                            desc_for_full.clone().unwrap_or_default()
                        } else {
                            snippet.clone()
                        }}
                    </p>
                    {description_truncated.then(|| view! {
                        <button
                            class="hit-description-toggle"
                            on:click=toggle_description
                        >
                            {move || if description_open.get() { "less" } else { "more" }}
                        </button>
                    })}
                </div>
            })}
            {move || {
                if !polar_open.get() {
                    return ().into_any();
                }
                let polar_view = if polar_loading.get() {
                    view! { <div class="hit-polar"><p>"Rendering polar…"</p></div> }.into_any()
                } else {
                    polar_svg.with(|s| match s {
                        Some(svg) if !svg.is_empty() => view! {
                            <div class="hit-polar" inner_html=svg.clone()></div>
                        }.into_any(),
                        Some(_) => view! {
                            <div class="hit-polar"><p class="empty">"No polar data."</p></div>
                        }.into_any(),
                        None => ().into_any(),
                    })
                };
                let image_view = if image_loading.get() {
                    view! { <div class="hit-image"><p>"Loading image…"</p></div> }.into_any()
                } else {
                    product_image.with(|i| match i {
                        Some(Some(img)) => {
                            let src = format!("data:{};base64,{}", img.mime, img.data_base64);
                            view! {
                                <div class="hit-image">
                                    <img src=src alt="Product picture"/>
                                </div>
                            }.into_any()
                        }
                        Some(None) => ().into_any(),
                        None => ().into_any(),
                    })
                };
                view! {
                    <div class="hit-expand">
                        <div class="hit-expand-row">
                            {image_view}
                            {polar_view}
                        </div>
                    </div>
                }
                .into_any()
            }}
        </li>
    }
    .into_any()
}
