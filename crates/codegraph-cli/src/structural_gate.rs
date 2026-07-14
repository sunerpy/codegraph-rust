//! Multilingual structural-question gate for the `prompt-hook` subcommand.
//!
//! Rust port of upstream `src/directory.ts` (#1126 `317e7f4` multilingual gate +
//! #1138 `713ab7a` right-bound `call`/`trace`/`affect`/`connect` stems), and
//! `extractCodeTokens` (`src/directory.ts:479-497`). The gate decides whether a
//! user prompt is a structural / flow / impact / "where-how" question worth
//! front-loading CodeGraph context for — the cheap, graph-free candidate check.
//!
//! ## Boundary emulation (no lookaround)
//!
//! Upstream expresses word boundaries with Unicode lookaround
//! (`(?<![\p{L}\p{N}_])…(?![\p{L}\p{N}_])`) because JS `\b` is ASCII-only and can
//! never bound an accented or non-Latin keyword (the #994 mechanism). Rust's
//! `regex` crate supports Unicode `\p{L}`/`\p{N}` classes and `(?i)` but has NO
//! lookbehind/lookahead, so the boundaries are emulated with **capture-class
//! consumption**: a leading `(?:^|[^\p{L}\p{N}_])` consumes the flanking
//! non-word char (or matches the string edge) instead of asserting it, and a
//! trailing `(?:[^\p{L}\p{N}_]|$)` does the same on the right. We only ever ask
//! `is_match`, never enumerate positions, so the extra consumed char is
//! irrelevant — this is behaviourally equivalent to the upstream lookaround.
//!
//! The term lists below are copied VERBATIM from upstream (WORDS
//! `directory.ts:255-310`, STEMS `directory.ts:316-406` incl. the #1138 bounded
//! stems, UNSEGMENTED `directory.ts:405-406`), preserving every inflection
//! alternation (`flows?`, `reach(?:es|ed)?`, `percors[oi]`), multi-word phrase
//! (`why does`, `di mana`, `làm sao`), and non-Latin / combining-mark entry
//! (Cyrillic, Greek, Devanagari `कैसे`/`कहाँ`/`संरचना`). The per-language
//! curation rationale (why a term is present/absent) is preserved as comments.

use std::sync::LazyLock;

use regex::Regex;

/// Structural keywords matched as EXACT words (boundary on both sides): short
/// or ambiguous tokens where prefix matching would false-positive ("flow" in
/// "flower", "path" in "pathological"). Grouped by language; a term appears once
/// even when several languages share it ("como" is Portuguese for how AND
/// unaccented-typed Spanish "cómo"). Verbatim from `directory.ts:255-310`.
const STRUCTURAL_WORDS: &[&str] = &[
    // English — the pre-#1126 list minus what moved to STRUCTURAL_STEMS: the
    // bare-stem entries never matched their own derived forms (`\barchitect\b`
    // can't match "architecture"), and "what calls" is subsumed by the "call" stem.
    "how",
    "where",
    "tracing",
    "flows?",
    "paths?",
    "reach(?:es|ed)?",
    "wired?",
    "breaks?",
    "why does",
    // French (où=where, flux=flow, chemin=path, casse=breaks)
    "comment",
    "où",
    "flux",
    "chemins?",
    "casse",
    // Spanish (cómo/como=how, dónde/donde=where, flujo=flow, ruta/camino=path,
    // rompe=breaks, llaman / quién llama = call(s) — bare "llama" is excluded:
    // it's also the animal/model name in English prompts)
    "cómo",
    "dónde",
    "donde",
    "flujos?",
    "rutas?",
    "caminos?",
    "rompe",
    "llaman",
    "quién llama",
    "quien llama",
    // Portuguese (como=how — also covers unaccented Spanish; onde=where,
    // fluxo=flow, caminho=path)
    "como",
    "onde",
    "fluxos?",
    "caminhos?",
    // German (wie=how, wo/woher/wohin=where, Pfad=path, Fluss/Ablauf=flow,
    // bricht/kaputt=breaks, ruft=calls, hängt=depends — "hängt … von X ab"
    // splits the separable verb "abhängen", so the "abhäng" stem can't catch it)
    "wie",
    "wo",
    "woher",
    "wohin",
    "pfade?",
    "fluss",
    "ablauf",
    "bricht",
    "kaputt",
    "ruft",
    "hängt",
    // Italian (dove=where, flusso=flow, percorso/i=path)
    "dove",
    "flusso",
    "percors[oi]",
    // Russian (как=how, где=where, путь/пути=path, работает=works)
    "как",
    "где",
    "путь",
    "пути",
    "работает",
    // Ukrainian (як=how, де=where, потік=flow — обліque cases reuse the RU
    // "поток" stem; працює=works)
    "як",
    "де",
    "потік",
    "працює",
    // Dutch (hoe=how, waar=where, roept=calls, werkt=works, aangeroepen=called —
    // the ge- participle escapes the "aanroep" stem)
    "hoe",
    "waar",
    "roept",
    "werkt",
    "aangeroepen",
    // Polish + Czech (jak=how — shared; gdzie/kde=where, cesta=path)
    "jak",
    "gdzie",
    "kde",
    "cesta",
    // Romanian (cum=how, unde=where; flux is shared with French)
    "cum",
    "unde",
    // Hungarian (hogyan=how, hol=where)
    "hogyan",
    "hol",
    // Turkish (nasıl=how, mimari=architecture, takip=trace/follow)
    "nasıl",
    "mimari",
    "takip",
    // Indonesian/Malay (bagaimana=how, di mana/dimana=where, alur=flow, jalur=path)
    "bagaimana",
    "di mana",
    "dimana",
    "alur",
    "jalur",
    // Vietnamese — spaced Latin with heavy diacritics, the exact class ASCII `\b`
    // breaks (làm sao/thế nào=how, ở đâu=where, gọi=call, phụ thuộc=depend,
    // ảnh hưởng=affect, kiến trúc=architecture, cấu trúc=structure, luồng=flow,
    // đường dẫn=path, hoạt động=works, giải thích=explain, theo dõi=trace)
    "làm sao",
    "thế nào",
    "ở đâu",
    "gọi",
    "phụ thuộc",
    "ảnh hưởng",
    "kiến trúc",
    "cấu trúc",
    "luồng",
    "đường dẫn",
    "hoạt động",
    "giải thích",
    "theo dõi",
    // Swedish / Danish / Norwegian (hur/hvordan=how, hvor=where, beror=depends,
    // flöde=flow)
    "hur",
    "hvordan",
    "hvor",
    "beror",
    "flöde",
    // Finnish (miten=how, missä=where, toimii=works)
    "miten",
    "missä",
    "toimii",
    // Greek (πώς=how, πού=where — accented forms only: unaccented πως/που are
    // ubiquitous conjunctions; καλεί=calls, δομή=structure, ροή=flow)
    "πώς",
    "πού",
    "καλεί",
    "δομή",
    "ροή",
    // Hindi (कैसे=how, कहाँ/कहां=where, कॉल=call, निर्भर=depends,
    // संरचना=structure, प्रवाह=flow)
    "कैसे",
    "कहाँ",
    "कहां",
    "कॉल",
    "निर्भर",
    "संरचना",
    "प्रवाह",
];

/// The lookaround-free trailing boundary: consumes a non-word char OR matches
/// the string edge. The #1138 English stems (`call`/`trace`/`affect`/`connect`)
/// embed this so ordinary words (callus, Connecticut, affectionate, Tracey)
/// can't false-fire the HIGH tier, while `calls`/`callback`/`connections` do.
const NOT_WORD_AFTER: &str = "(?:[^\\p{L}\\p{N}_]|$)";

/// Structural keyword STEMS matched as word PREFIXES (boundary on the left
/// only), so derived forms match without enumerating each: "architect" fires on
/// architecture/architectural, "depend" on depends/dependency/dependencies,
/// "вызыва" on вызывает/вызывается. Mid-word occurrences stay excluded —
/// "restructure"/"independent" don't fire — so precision stays close to the
/// exact-word class. A stem with ordinary-English completions instead
/// enumerates its structural suffixes and re-asserts the RIGHT boundary (the
/// four bounded English entries below, #1138). Verbatim from `directory.ts`
/// (#1126 + #1138); the four bounded entries carry `NOT_WORD_AFTER` as a
/// branch-local trailing boundary (NOT `$` anchors — a `^…$` branch inside the
/// combined alternation would only fire when the stem is the ENTIRE prompt).
static STRUCTURAL_STEMS: LazyLock<Vec<String>> = LazyLock::new(|| {
    let mut v: Vec<String> = Vec::new();
    // English + the Latin-script languages that share the spelling (French
    // architecture/structure/trace/impact, Spanish depende/implementa/impacto, …).
    // call/trace/affect/connect are NOT safe as open prefixes — callus,
    // calligraphy, Connecticut, connective, affectionate, Tracey are ordinary
    // words that would false-fire the full-explore tier (#1138) — so they carry
    // an enumerated suffix set + right boundary. "tracing" lives in
    // STRUCTURAL_WORDS (the e is dropped, so no trace-prefix form matches it).
    for s in [
        "architect",
        "structur",
        "depend",
        "implement",
        "impact",
        "explain",
    ] {
        v.push(s.to_string());
    }
    v.push(format!(
        "call(?:s|ing|ed|ers?|backs?|able|sites?)?{NOT_WORD_AFTER}"
    ));
    v.push(format!("trace(?:s|d|rs?)?{NOT_WORD_AFTER}"));
    v.push(format!("affect(?:s|ed|ing)?{NOT_WORD_AFTER}"));
    v.push(format!(
        "connect(?:s|ed|ing|ions?|ors?|ivity)?{NOT_WORD_AFTER}"
    ));
    for s in STRUCTURAL_STEMS_TAIL {
        v.push((*s).to_string());
    }
    v
});

/// The open-prefix stems for all non-English languages (verbatim from
/// `directory.ts:334-406`); appended after the bounded English entries.
const STRUCTURAL_STEMS_TAIL: &[&str] = &[
    // French (appel(le)=call, dépend=depends, implément(e)=implement,
    // connex(ion)=connection, expliqu(e)=explain, fonctionn(e/ement)=works)
    "appel",
    "dépend",
    "implément",
    "connex",
    "expliqu",
    "fonctionn",
    // Spanish (llamad(a)=call, afect(a)=affect, conect(a)/conexi(ón)=connect,
    // arquitec(tura)=architecture, estructur(a)=structure, funcion(a)=works,
    // traza(r)=trace, explica=explain)
    "llamad",
    "afect",
    "conect",
    "conexi",
    "arquitec",
    "estructur",
    "funcion",
    "traza",
    "explica",
    // Portuguese (chama(da)=call, afeta=affect, arquitet(ura)=architecture,
    // estrutur(a)=structure, quebra(do)=breaks)
    "chama",
    "afeta",
    "arquitet",
    "estrutur",
    "quebra",
    // German (abhäng(t)=depend, Auswirkung=impact, beeinfluss(t)=affect,
    // verbind(et)=connect, Architektur, Struktur, funktionier(t)=works,
    // Aufruf/aufgerufen=call, erklär(t)=explain, verfolg(en)=trace)
    "abhäng",
    "auswirkung",
    "beeinfluss",
    "verbind",
    "architekt",
    "struktur",
    "funktionier",
    "aufruf",
    "aufgerufen",
    "erklär",
    "verfolg",
    // Italian (chiam(a/ata)=call, dipend(e/enza)=depend, impatt(o)=impact,
    // connett(e)/conness(ione)=connect, architett(ura), struttur(a),
    // funzion(a/amento)=works, tracci(a)=trace, spiega(mi)=explain)
    "chiam",
    "dipend",
    "impatt",
    "connett",
    "conness",
    "architett",
    "struttur",
    "funzion",
    "tracci",
    "spiega",
    // Russian (вызыва(ет)=calls, завис(ит)=depends, влия(ет)=affects,
    // реализ(ация)=implementation, структур(а), архитектур(а),
    // трассир(овка)=trace, лома(ет)=breaks, объясн(и)=explain, поток=flow)
    "вызыва",
    "завис",
    "влия",
    "реализ",
    "структур",
    "архитектур",
    "трассир",
    "лома",
    "объясн",
    "поток",
    // Ukrainian — і/и spellings diverge from Russian (виклика(є)=calls,
    // залеж(ить)=depends, вплива(є)=affects, архітектур(а), реаліз(ація),
    // поясн(и)=explain, шлях(у)=path; структур(а) is shared with Russian)
    "виклика",
    "залеж",
    "вплива",
    "архітектур",
    "реаліз",
    "поясн",
    "шлях",
    // Dutch (aanroep(en)=call, afhankelijk(heid)=depends, beïnvloed(t)=affects,
    // structuur — "structur" can't reach the uu; uitleg(gen)=explain)
    "aanroep",
    "afhankelijk",
    "beïnvloed",
    "structuur",
    "uitleg",
    // Polish (wywoł(uje)=calls, zależ(y)=depends, wpływ(a)=affects/impact,
    // przepływ=flow, ścieżk(a)=path, działa(nie)=works, wyjaśni(j)=explain,
    // śledz(enie)=trace; architektura/struktura fire via the German stems)
    "wywoł",
    "zależ",
    "wpływ",
    "przepływ",
    "ścieżk",
    "działa",
    "wyjaśni",
    "śledz",
    // Czech (volá(ní)=calls, závis(í)=depends, ovlivň(uje)=affects,
    // funguj(e)=works, vysvětl(i)=explain)
    "volá",
    "závis",
    "ovlivň",
    "funguj",
    "vysvětl",
    // Romanian (apel(ează)=calls, depind(e)=depends — i not e, so "depend" misses
    // it; arhitectur(a) — no c; funcțion(ează)=works, explică=explain)
    "apel",
    "depind",
    "arhitectur",
    "funcțion",
    "explică",
    // Hungarian (hív(ja)=calls, függ(őség)=depends, működ(ik)=works,
    // struktúr(a) — ú escapes "struktur"; magyaráz(d)=explain;
    // architektúra fires via the German stem)
    "hív",
    "függ",
    "működ",
    "struktúr",
    "magyaráz",
    // Turkish — agglutinative, so stems beat exact words (nere(de/ye/den)=where,
    // çağır/çağrı=call, bağıml(ı)=depends, bağlant(ı)=connection, akış(ı)=flow,
    // etkile(r)/etkisi=affects/impact)
    "nere",
    "çağır",
    "çağrı",
    "bağıml",
    "bağlant",
    "akış",
    "etkile",
    "etkisi",
    // Indonesian/Malay — me-/di-/ber- prefixes block a bare stem, so affixed
    // forms are listed too (panggil(an)/memanggil/dipanggil=call,
    // bergantung/tergantung=depends, pengaruh/mempengaruhi/memengaruhi=affect,
    // arsitektur=architecture, fungsi/berfungsi=works,
    // jelaskan/menjelaskan=explain)
    "panggil",
    "memanggil",
    "dipanggil",
    "bergantung",
    "tergantung",
    "pengaruh",
    "mempengaruhi",
    "memengaruhi",
    "arsitektur",
    "fungsi",
    "berfungsi",
    "jelaskan",
    "menjelaskan",
    // Swedish / Danish / Norwegian (anrop(ar)=calls, påverk(ar)/påvirk(er)=affects,
    // afhæng(er)/avheng(er)=depends, förklar(a)/forklar=explain,
    // arkitektur — k not ch; funger(ar/er)=works)
    "anrop",
    "påverk",
    "påvirk",
    "afhæng",
    "avheng",
    "förklar",
    "forklar",
    "arkitektur",
    "funger",
    // Finnish (kutsu(u)=calls, riippu(u)=depends, arkkitehtuur(i),
    // rakente(en)=structure, selit(ä)=explain)
    "kutsu",
    "riippu",
    "arkkitehtuur",
    "rakente",
    "selit",
    // Greek — accented and unaccented stem spellings both occur
    // (εξαρτ(άται)=depends, επηρε(άζει)=affects, αρχιτεκτονικ(ή),
    // διαδρομ(ή)=path, εξηγ/εξήγ(ησε)=explain)
    "εξαρτ",
    "επηρε",
    "αρχιτεκτονικ",
    "διαδρομ",
    "εξηγ",
    "εξήγ",
    // Hindi (समझा(ओ/इए)=explain, आर्किटेक्चर=architecture)
    "समझा",
    "आर्किटेक्चर",
];

/// Structural keywords matched as bare SUBSTRINGS, for languages where a
/// boundary can't be relied on: scripts with no word separators (Chinese —
/// simplified AND traditional; the original #994 set was simplified-only —
/// Japanese, Thai), Korean (spaced, but particles attach directly to the noun:
/// 구조가/구조를), and Arabic / Farsi / Hebrew (spaced, but proclitics attach to
/// the word: وكيف "and-how", והמבנה "and-the-structure"). JS's `\b` can never
/// fire between Han characters, which was issue #994.
///
/// KNOWN, ACCEPTED false-positive class (#1140): substring matching cannot see
/// homograph compounds — Korean 구조 (structure) also fires inside 구조대
/// (rescue squad). Verified unfixable at this layer: ICU word segmentation
/// returns 구조대 and the particle form 구조가 (which the gate MUST keep
/// matching) as equally opaque single segments, and a 구조대 denylist would
/// break 구조대로 ("according to the structure"), a legitimate structural
/// prompt. The miss rate this design avoids (silently no-op'ing every prompt in
/// these languages, #994) outweighs the occasional off-domain fire.
/// Verbatim from `directory.ts:405-406`.
const STRUCTURAL_UNSEGMENTED: &str = "如何|怎么|怎麼|在哪|哪里|哪裡|追踪|跟踪|追蹤|追跡|トレース|流程|流向|流れ|路径|路徑|経路|调用|調用|呼び出|依赖|依賴|依存|影响|影響|实现|實現|実装|架构|架構|アーキテクチャ|结构|結構|構造|介绍|介紹|解析|分析|原理|机制|機制|仕組み|説明|動作|どうやって|どのように|어떻게|어디|호출|흐름|경로|의존|영향|구현|구조|아키텍처|추적|동작|작동|설명|كيف|أين|اين|يستدعي|استدعاء|يعتمد|تعتمد|يؤثر|تأثير|معماري|بنية|هيكل|تدفق|مسار|تتبع|يعمل|تعمل|اشرح|شرح|چگونه|چطور|کجا|فراخوان|وابسته|تأثیر|معماری|ساختار|مسیر|توضیح|איך|איפה|קורא|תלוי|משפיע|ארכיטקטור|מבנה|זרימה|נתיב|הסבר|อย่างไร|ยังไง|ที่ไหน|เรียกใช้|ขึ้นอยู่กับ|ผลกระทบ|สถาปัตยกรรม|โครงสร้าง|เส้นทาง|ติดตาม|ทำงาน|อธิบาย";

/// Doc/data/asset file extensions — a `name.ext` of this kind is a file
/// reference, not a code symbol, so it must not trip the member-access signal.
/// Verbatim from `directory.ts` `DOC_DATA_EXT`.
static DOC_DATA_EXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\.(md|markdown|txt|rst|json|ya?ml|toml|lock|csv|tsv|log|ini|cfg|conf|env|xml|html?|png|jpe?g|gif|svg|pdf)$",
    )
    .expect("DOC_DATA_EXT regex is valid")
});

/// WORDS matched with a boundary on BOTH sides (lookaround-free): a leading
/// `(?:^|[^\p{L}\p{N}_])` and a trailing `(?:[^\p{L}\p{N}_]|$)` consume the
/// flanking non-word char (or match the string edge).
static STRUCTURAL_WORDS_RE: LazyLock<Regex> = LazyLock::new(|| {
    let body = STRUCTURAL_WORDS.join("|");
    Regex::new(&format!(
        r"(?i)(?:^|[^\p{{L}}\p{{N}}_])(?:{body})(?:[^\p{{L}}\p{{N}}_]|$)"
    ))
    .expect("STRUCTURAL_WORDS regex is valid")
});

/// STEMS matched as word PREFIXES (left boundary only). The four #1138 English
/// entries carry their own trailing boundary inside the alternation.
static STRUCTURAL_STEMS_RE: LazyLock<Regex> = LazyLock::new(|| {
    let body = STRUCTURAL_STEMS.join("|");
    Regex::new(&format!(r"(?i)(?:^|[^\p{{L}}\p{{N}}_])(?:{body})"))
        .expect("STRUCTURAL_STEMS regex is valid")
});

/// UNSEGMENTED — a plain alternation with NO boundaries (separator-less /
/// proclitic scripts), exactly as upstream matches them without `\b`.
static STRUCTURAL_UNSEGMENTED_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(STRUCTURAL_UNSEGMENTED).expect("STRUCTURAL_UNSEGMENTED regex is valid")
});

/// Does `prompt` contain an explicit structural keyword? A keyword is a strong,
/// self-contained signal, so the front-load hook fires on it directly — no
/// graph check needed. (A *code-token* match, by contrast, is only a candidate
/// the hook verifies against the graph first; see [`extract_code_tokens`].)
/// Coverage is multilingual (#994, #1126): the ~29 languages with the largest
/// developer populations, across Latin, Cyrillic, Greek, CJK, Hangul, Arabic,
/// Hebrew, Thai, and Devanagari scripts. Mirrors upstream `hasStructuralKeyword`.
pub fn has_structural_keyword(prompt: &str) -> bool {
    !prompt.is_empty()
        && (STRUCTURAL_WORDS_RE.is_match(prompt)
            || STRUCTURAL_STEMS_RE.is_match(prompt)
            || STRUCTURAL_UNSEGMENTED_RE.is_match(prompt))
}

/// Identifier-run regex: `[A-Za-z_$][\w$]*` (a token starting with a letter,
/// underscore, or `$`, then word chars or `$`).
static IDENT_RUN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z_$][\w$]*").expect("ident-run regex is valid"));

/// Inner camelCase transition `[a-z][A-Z]`.
static INNER_CAMEL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[a-z][A-Z]").expect("inner-camel regex is valid"));

/// Flanked underscore `[A-Za-z0-9]_[A-Za-z0-9]`.
static FLANKED_UNDERSCORE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9]_[A-Za-z0-9]").expect("flanked-underscore regex is valid")
});

/// Call form: an identifier directly before `(`.
static CALL_FORM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([A-Za-z_$][\w$]*)\(").expect("call-form regex is valid"));

/// Member access on identifiers: `a.b`.
static MEMBER_ACCESS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([A-Za-z_$][\w$]*)\.([A-Za-z_$][\w$]*)").expect("member-access regex is valid")
});

/// Identifier-shaped tokens in `prompt` — camelCase / PascalCase-with-inner-cap,
/// snake_case, a `name(` call, or the two sides of an `a.b` member access.
/// Naming a symbol is a code question whatever the surrounding human language.
///
/// These are *candidates*, not a verdict: a tech brand like `JavaScript` or
/// `GitHub` is identifier-shaped too, so the front-load hook checks each token
/// against the actual index and only fires when one is a real symbol here. A
/// doc/data filename ("README.md") is excluded from the member-access form.
///
/// A **bare single PascalCase word** like `Counter` matches NONE of these — no
/// inner `lower→Upper`, no underscore, no `(`, no `.` — so it is NOT extracted
/// (faithful to upstream `extractCodeTokens`, `directory.ts:479-497`). Order is
/// deterministic: tokens are returned in first-seen appearance order.
pub fn extract_code_tokens(prompt: &str) -> Vec<String> {
    if prompt.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let push = |s: &str, out: &mut Vec<String>| {
        if !out.iter().any(|x| x == s) {
            out.push(s.to_string());
        }
    };

    // camelCase / PascalCase-with-inner-cap or snake_case: a whole identifier
    // run with an inner lower→upper transition or an underscore flanked by
    // alphanumerics.
    for m in IDENT_RUN_RE.find_iter(prompt) {
        let w = m.as_str();
        if INNER_CAMEL_RE.is_match(w) || FLANKED_UNDERSCORE_RE.is_match(w) {
            push(w, &mut out);
        }
    }
    // call form: an identifier directly before '('.
    for caps in CALL_FORM_RE.captures_iter(prompt) {
        let name = caps.get(1).map_or("", |m| m.as_str()).to_string();
        push(&name, &mut out);
    }
    // member access on identifiers (user.login) — but not a doc/data filename.
    for caps in MEMBER_ACCESS_RE.captures_iter(prompt) {
        let whole = caps.get(0).map_or("", |m| m.as_str());
        if !DOC_DATA_EXT.is_match(whole) {
            let a = caps.get(1).map_or("", |m| m.as_str()).to_string();
            let b = caps.get(2).map_or("", |m| m.as_str()).to_string();
            push(&a, &mut out);
            push(&b, &mut out);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_fires_on_english_structural() {
        assert!(has_structural_keyword("how does this work"));
        assert!(has_structural_keyword("where is the router"));
    }

    #[test]
    fn gate_fires_on_non_english() {
        assert!(has_structural_keyword("comment marche le routeur")); // FR
        assert!(has_structural_keyword("这个模块如何工作")); // zh
        assert!(has_structural_keyword("이건 어떻게 작동하나요")); // ko
        assert!(has_structural_keyword("где находится роутер")); // ru
    }

    #[test]
    fn gate_fires_on_inflected_and_phrase() {
        assert!(has_structural_keyword("what are the data flows here"));
        assert!(has_structural_keyword("this reaches the handler"));
        assert!(has_structural_keyword("why does the counter reset"));
    }

    #[test]
    fn gate_fires_on_devanagari() {
        assert!(has_structural_keyword("यह कैसे काम करता है")); // how
        assert!(has_structural_keyword("इसकी संरचना क्या है")); // structure
    }

    #[test]
    fn gate_mid_word_exclusion() {
        // "flow" inside "flower", "path" inside "pathological" must NOT fire via
        // the WORDS class (boundary char-class, not per-run equality).
        assert!(!STRUCTURAL_WORDS_RE.is_match("flower shop"));
        assert!(!STRUCTURAL_WORDS_RE.is_match("pathological case"));
    }

    #[test]
    fn gate_fires_on_calls_in_sentence() {
        // Branch-local boundaries compose inside the combined STEMS regex.
        assert!(has_structural_keyword("what calls Counter"));
        assert!(has_structural_keyword("show me the callback"));
        assert!(has_structural_keyword("list the call sites"));
        assert!(has_structural_keyword("how many callers"));
        assert!(has_structural_keyword("trace the traces"));
        assert!(has_structural_keyword("what affects this"));
        assert!(has_structural_keyword("show connections"));
    }

    #[test]
    fn gate_no_false_fire_on_ordinary_words() {
        // #1138 right-bounding: ordinary words with these prefixes must NOT fire.
        assert!(!has_structural_keyword("about Connecticut weather"));
        assert!(!has_structural_keyword("i have a callus on my foot"));
        assert!(!has_structural_keyword("calligraphy is an art"));
        assert!(!has_structural_keyword("this connective tissue"));
        assert!(!has_structural_keyword("an affectionate dog"));
        assert!(!has_structural_keyword("Tracey went home"));
    }

    #[test]
    fn gate_silent_on_plain_prose() {
        assert!(!has_structural_keyword("please fix this typo"));
        assert!(!has_structural_keyword("rename the variable"));
    }

    #[test]
    fn extract_code_tokens_rules() {
        // inner camelCase / PascalCase-with-inner-cap
        assert!(extract_code_tokens("look at OrderService").contains(&"OrderService".to_string()));
        assert!(extract_code_tokens("the getUserId helper").contains(&"getUserId".to_string()));
        // snake_case flanked underscore
        assert!(extract_code_tokens("call get_user now").contains(&"get_user".to_string()));
        // call form
        assert!(extract_code_tokens("Counter() creates one").contains(&"Counter".to_string()));
        // member access
        let toks = extract_code_tokens("user.login please");
        assert!(toks.contains(&"user".to_string()));
        assert!(toks.contains(&"login".to_string()));
    }

    #[test]
    fn extract_code_tokens_bare_pascalcase_is_not_a_token() {
        // A bare single PascalCase word is NOT a token (upstream faithfulness).
        assert!(extract_code_tokens("Counter").is_empty());
        assert!(extract_code_tokens("look at Counter here").is_empty());
    }

    #[test]
    fn extract_code_tokens_doc_filename_excluded() {
        // README.md is a doc filename — not a member-access symbol.
        assert!(extract_code_tokens("open README.md").is_empty());
        assert!(extract_code_tokens("edit config.json now").is_empty());
    }

    #[test]
    fn extract_code_tokens_plain_prose_none() {
        assert!(extract_code_tokens("please fix this typo").is_empty());
    }
}
