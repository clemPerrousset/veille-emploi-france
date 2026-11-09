(function () {
  "use strict";
  var D = window.DATA, AI = window.AI;
  var DAY = 864e5;
  function $(s) { return document.querySelector(s); }
  function ms(d) { return Date.parse(d + "T00:00:00Z"); }
  var nfPct = new Intl.NumberFormat("fr-FR", { minimumFractionDigits: 1, maximumFractionDigits: 1, signDisplay: "always" });
  var nf1 = new Intl.NumberFormat("fr-FR", { minimumFractionDigits: 1, maximumFractionDigits: 1 });
  var nf0 = new Intl.NumberFormat("fr-FR", { maximumFractionDigits: 0 });
  function pct(v) { return nfPct.format(v) + " %"; }
  function dateFr(t, long) {
    return new Date(t).toLocaleDateString("fr-FR", long ? { day: "numeric", month: "short", year: "numeric", timeZone: "UTC" } : { month: "short", year: "numeric", timeZone: "UTC" });
  }
  function lowerBound(a, v) { var lo = 0, hi = a.length; while (lo < hi) { var m = (lo + hi) >> 1; if (a[m] < v) lo = m + 1; else hi = m; } return lo; }
  function closest(a, v) { var i = lowerBound(a, v); if (i > 0 && (i === a.length || v - a[i - 1] < a[i] - v)) i--; return i; }

  /* ---------- préparation des données ---------- */
  var secX = D.sectors.dates.map(ms);
  var cats = Object.keys(D.sectors.series);
  var GROUPS = {};
  cats.forEach(function (k) { var g = (AI[k] || {}).groupe || "Autres"; (GROUPS[g] = GROUPS[g] || []).push(k); });
  var ITEMS = {}; // clé -> {key,label,xs,ys,kind,...}
  cats.forEach(function (k) {
    ITEMS["c:" + k] = { key: "c:" + k, label: (AI[k] || {}).fr || k, xs: secX, ys: D.sectors.series[k], kind: "cat", expo: (AI[k] || {}).expo, tech: !!(AI[k] || {}).tech };
  });
  Object.keys(GROUPS).forEach(function (g) {
    var m = GROUPS[g];
    var ys = secX.map(function (_, i) { var s = 0; m.forEach(function (k) { s += D.sectors.series[k][i]; }); return s / m.length; });
    var ex = m.reduce(function (s, k) { return s + (AI[k] || {}).expo; }, 0) / m.length;
    ITEMS["g:" + g] = { key: "g:" + g, label: g, xs: secX, ys: ys, kind: "grp", expo: ex, sub: m.length + " catégories" };
  });
  var aggX = D.aggregate["total postings"].dates.map(ms);
  ITEMS.total = { key: "total", label: "Offres d'emploi, ensemble de la France", xs: aggX, ys: D.aggregate["total postings"].values, kind: "tot" };
  ITEMS.newp = { key: "newp", label: "Nouvelles offres publiées", xs: D.aggregate["new postings"].dates.map(ms), ys: D.aggregate["new postings"].values, kind: "tot" };
  var MACRO_COL = { chomage: "#0f766e", pib: "#7c3aed", exports: "#0891b2", imports: "#b45309", ft_offres: "#be185d", ft_dei: "#4d7c0f" };
  var VAC_LABEL = { F: "Construction", GTI: "Commerce, transports, hôtellerie-restauration", L: "Immobilier", BTE: "Industrie", J: "Information & communication (tech)", P_Q: "Enseignement, santé, action sociale", R_S: "Arts, loisirs & autres services", K: "Finance & assurance", M_N: "Services spécialisés & de soutien" };
  Object.keys(D.vacants || {}).forEach(function (c) {
    var v = D.vacants[c];
    ITEMS["v:" + c] = { key: "v:" + c, label: VAC_LABEL[c] || v.label, xs: v.points.map(function (p) { return ms(p[0]); }), ys: v.points.map(function (p) { return p[1]; }), kind: "vac", unit: "lvl", sub: "trimestriel" };
  });
  function isPts(it) { return it.unit === "pt" || it.unit === "lvl"; }
  /* métiers fins (ROME), trimestriels, API France Travail « Marché du travail » */
  var R = window.ROME, ROME_KEYS = [], ROME_BY_LABEL = {}, OFF_CLEAN = {};
  var AIR = window.AIROME || null; // exposition IA estimée par métier ROME (data/ai_rome.js)
  var AI_NOTE = "Les scores d'exposition par métier précis sont l'estimation d'un modèle de langage (Claude) à partir de l'intitulé du métier : ni des données officielles, ni les scores de la Banque de France (non publiés). À lire comme un ordre de grandeur.";
  // cumul sur 4 trimestres glissants d'une série trimestrielle (ignore les fenêtres incomplètes)
  function cum4(arr, x) {
    var xs = [], ys = [];
    for (var i = 3; i < arr.length; i++) {
      var s = 0, ok = true;
      for (var j = i - 3; j <= i; j++) { if (arr[j] == null) { ok = false; break; } s += arr[j]; }
      if (ok && s > 0) { xs.push(x[i]); ys.push(s); }
    }
    return { xs: xs, ys: ys };
  }
  if (R) {
    var qx = R.periods.map(function (p) { return Date.UTC(+p.slice(0, 4), (+p.slice(5) - 1) * 3 + 1, 15); });
    // Nettoyage : un trimestre bien plus haut (x3 et +500) que les mêmes trimestres des autres années
    // est un pic isolé (artefact de saisie, import en masse) : on le remplace par leur médiane.
    var cleanQ = function (o) {
      return o.map(function (v, i) {
        if (v == null) return v;
        var peers = [-8, -4, 4, 8].map(function (d) { return o[i + d]; }).filter(function (x) { return x != null; });
        if (peers.length < 3) return v;
        peers.sort(function (a, b) { return a - b; });
        var med = (peers[Math.floor((peers.length - 1) / 2)] + peers[Math.ceil((peers.length - 1) / 2)]) / 2;
        return (v > 3 * med && v - med >= 500) ? med : v;
      });
    };
    Object.keys(R.items).forEach(function (c) {
      var it = R.items[c], o = cleanQ(it.o);
      OFF_CLEAN[c] = o;
      // cumul sur 4 trimestres glissants : supprime la saisonnalité (cueillette, petite enfance…)
      var cu = cum4(o, qx), xs = cu.xs, ys = cu.ys;
      if (xs.length < 4) return;
      ITEMS["r:" + c] = { key: "r:" + c, label: it.l, xs: xs, ys: ys, kind: "rome", sub: "cumul 4 trim. · " + c, code: c };
      ROME_KEYS.push("r:" + c);
      ROME_BY_LABEL[it.l + " (" + c + ")"] = "r:" + c;
    });
  }
  var ROME_MIN = 4000; // offres cumulées sur 4 trimestres au départ : en dessous, la variation n'est que du bruit
  /* indicateurs du bloc tech : offres, demandeurs inscrits, embauches, ratio (fetch_tech.py) */
  var TECHD = window.TECH, TECH_CODES = [];
  var TECH_METRICS = { off: "Offres (4 trim.)", dem: "Demandeurs inscrits", emb: "Embauches (4 trim.)", ratio: "Offres pour 100 demandeurs" };
  if (R && TECHD) {
    var tq = TECHD.periods.map(function (p) { return Date.UTC(+p.slice(0, 4), (+p.slice(5) - 1) * 3 + 1, 15); });
    Object.keys(TECHD.items).forEach(function (c) {
      var it = TECHD.items[c];
      var offArr = TECHD.periods.map(function (p) { var k = R.periods.indexOf(p); return k >= 0 && OFF_CLEAN[c] ? OFF_CLEAN[c][k] : null; });
      var offC = cum4(offArr, tq), embC = cum4(it.e, tq), dx = [], dy = [], rx = [], ry = [];
      it.d.forEach(function (v, i) { if (v != null && v > 0) { dx.push(tq[i]); dy.push(v); } });
      for (var i = 3; i < tq.length; i++) {
        var o4 = 0, d4 = 0, ok = true;
        for (var j = i - 3; j <= i; j++) { if (offArr[j] == null || it.d[j] == null) { ok = false; break; } o4 += offArr[j]; d4 += it.d[j]; }
        if (ok && d4 > 0) { rx.push(tq[i]); ry.push(o4 / (d4 / 4) * 100); }
      }
      var add = function (m, xs, ys, unit) {
        if (xs.length >= 2) ITEMS["t:" + m + ":" + c] = { key: "t:" + m + ":" + c, label: it.l, xs: xs, ys: ys, kind: "rome", sub: c, code: c, unitLabel: unit, invert: m === "dem" };
      };
      add("off", offC.xs, offC.ys, "offres sur 4 trim."); add("dem", dx, dy, "demandeurs inscrits (cat. A)");
      add("emb", embC.xs, embC.ys, "embauches sur 4 trim."); add("ratio", rx, ry, "offres pour 100 demandeurs");
      if (ITEMS["t:off:" + c]) TECH_CODES.push(c);
    });
    TECH_CODES.sort(function (a, b) { var A = ITEMS["t:off:" + a].ys, B = ITEMS["t:off:" + b].ys; return B[B.length - 1] - A[A.length - 1]; });
  }
  if (AIR) Object.keys(ITEMS).forEach(function (k) { var it = ITEMS[k]; if (it.code && AIR[it.code] != null) it.expo = AIR[it.code]; });
  /* cinquièmes d'exposition : somme des offres des métiers notés, 5 groupes de même effectif */
  var QCACHE = null;
  function buildQ() {
    if (QCACHE) return QCACHE;
    var maxLen = 0; ROME_KEYS.forEach(function (k) { if (ITEMS[k].xs.length > maxLen) maxLen = ITEMS[k].xs.length; });
    var pool = ROME_KEYS.filter(function (k) { return ITEMS[k].expo != null && ITEMS[k].xs.length === maxLen; });
    pool.sort(function (a, b) { return ITEMS[a].expo - ITEMS[b].expo || (a < b ? -1 : 1); });
    var xs = ITEMS[pool[0]].xs, out = [], per = Math.ceil(pool.length / 5);
    var names = ["Cinquième le moins exposé", "2ᵉ cinquième", "3ᵉ cinquième", "4ᵉ cinquième", "Cinquième le plus exposé"];
    for (var g = 0; g < 5; g++) {
      var mem = pool.slice(g * per, (g + 1) * per); if (!mem.length) continue;
      var ys = xs.map(function (_, i) { return mem.reduce(function (s, k) { return s + ITEMS[k].ys[i]; }, 0); });
      var avg = mem.reduce(function (s, k) { return s + ITEMS[k].expo; }, 0) / mem.length;
      var it = { key: "q:" + g, label: names[g], xs: xs, ys: ys, kind: "rome", expo: avg, sub: mem.length + " métiers", unitLabel: "offres sur 4 trim." };
      ITEMS[it.key] = it; out.push(it);
    }
    QCACHE = out; QCACHE.n = pool.length; return out;
  }
  function techAgg(m) { // somme des métiers tech, date par date
    var acc = {};
    TECH_CODES.forEach(function (c) { var it = ITEMS["t:" + m + ":" + c]; if (it) it.xs.forEach(function (x, i) { acc[x] = (acc[x] || 0) + it.ys[i]; }); });
    var xs = Object.keys(acc).map(Number).sort(function (a, b) { return a - b; });
    return { xs: xs, ys: xs.map(function (x) { return acc[x]; }) };
  }
  function qLabel(x) { var d = new Date(x); return "T" + (Math.floor(d.getUTCMonth() / 3) + 1) + " " + d.getUTCFullYear(); }
  Object.keys(D.macro).forEach(function (k) {
    var m = D.macro[k];
    ITEMS["m:" + k] = { key: "m:" + k, label: m.label, xs: m.points.map(function (p) { return ms(p[0]); }), ys: m.points.map(function (p) { return p[1]; }), kind: "macro", unit: k === "chomage" ? "pt" : "%", sub: m.freq, color: MACRO_COL[k] };
  });
  var LAST = Math.max(secX[secX.length - 1], aggX[aggX.length - 1]);
  var FIRST = ms("2020-02-01");
  var PALETTE = ["#2563eb", "#e11d48", "#16a34a", "#d97706", "#a21caf", "#0e7490"];
  var MARKET_COL = "#64748b";

  /* ---------- état ---------- */
  var PERIODS = [["1 mois", 30], ["3 mois", 91], ["6 mois", 182], ["1 an", 365], ["2 ans", 730], ["3 ans", 1095], ["Depuis 2020", "all"]];
  var VIEWS = [
    { id: "global", label: "Global", title: "Emploi en France" },
    { id: "empUp", label: "Top 5 emplois en montée", title: "Métiers en montée" },
    { id: "empDown", label: "Top 5 emplois en descente", title: "Métiers en descente" },
    { id: "secUp", label: "Top 5 secteurs en montée", title: "Secteurs en montée" },
    { id: "secDown", label: "Top 5 secteurs en descente", title: "Secteurs en descente" },
    { id: "aiExp", label: "Top 5 métiers exposés à l'IA", title: "Les plus exposés à l'IA (estimation)" },
    { id: "aiUp", label: "Top 5 métiers IA en montée", title: "Exposés à l'IA et en montée" },
    { id: "vac", label: "Difficultés de recrutement", title: "Emplois vacants par secteur" },
    { id: "tech", label: "Spécial tech", title: "Métiers de la tech" },
    { id: "romeUp", label: "Métiers précis en montée", title: "Métiers précis (ROME) en montée", rome: true },
    { id: "romeDown", label: "Métiers précis en descente", title: "Métiers précis (ROME) en descente", rome: true },
    { id: "romeAi", label: "Métiers précis exposés à l'IA", title: "Métiers précis les plus exposés à l'IA (estimation)", rome: true, ai: true },
    { id: "romeAiUp", label: "Métiers précis IA en montée", title: "Métiers précis exposés à l'IA et en montée", rome: true, ai: true },
    { id: "romeQ", label: "Offres par exposition à l'IA", title: "Cinq niveaux d'exposition à l'IA", rome: true, ai: true },
    { id: "romeTech", label: "Tech : tableau de bord", title: "Métiers du numérique (ROME)", rome: true, tech: true },
    { id: "mine", label: "Mon suivi", title: "Mes métiers" }
  ].filter(function (v) { return (!v.rome || ROME_KEYS.length) && (!v.tech || TECH_CODES.length) && (!v.ai || AIR); });
  var state = { view: "global", period: 365, custom: "", rebase: "window", macro: [], mine: ["c:Software Development"], techMetric: "off" };
  try { var sv = JSON.parse(localStorage.getItem("veille-emploi") || "null"); if (sv) Object.keys(state).forEach(function (k) { if (sv[k] !== undefined) state[k] = sv[k]; }); } catch (e) {}
  if (!VIEWS.some(function (v) { return v.id === state.view; })) state.view = "global";
  state.mine = state.mine.filter(function (k) { return ITEMS[k]; });
  function save() { try { localStorage.setItem("veille-emploi", JSON.stringify(state)); } catch (e) {} }

  /* ---------- calculs ---------- */
  function startMs() {
    if (state.custom) return Math.min(Math.max(FIRST, ms(state.custom)), LAST - 7 * DAY);
    if (state.period === "all") return FIRST;
    return Math.max(FIRST, LAST - state.period * DAY);
  }
  function measure(item, start) {
    var i0 = lowerBound(item.xs, start);
    // série trimestrielle qui s'arrête avant la période choisie : on compare ses deux derniers points
    if (i0 >= item.xs.length - 1 && (item.kind === "rome" || item.kind === "vac") && item.xs.length >= 2) i0 = item.xs.length - 2;
    if (i0 >= item.xs.length) return null;
    var first = item.ys[i0], last = item.ys[item.ys.length - 1];
    var delta = isPts(item) ? last - first : (last / first - 1) * 100;
    return { delta: delta, last: last, n: item.xs.length - i0 };
  }
  function rank(keys, start, n, dir) {
    var arr = keys.map(function (k) { var m = measure(ITEMS[k], start); return m ? { key: k, d: m.delta } : null; }).filter(Boolean);
    arr.sort(function (a, b) { return dir === "up" ? b.d - a.d : a.d - b.d; });
    return arr.slice(0, n).map(function (a) { return a.key; });
  }
  function avgDelta(keys, start) {
    var v = keys.map(function (k) { return measure(ITEMS[k], start).delta; });
    return v.reduce(function (s, x) { return s + x; }, 0) / v.length;
  }
  var ALL_CATS = cats.map(function (k) { return "c:" + k; });
  var ALL_GRPS = Object.keys(GROUPS).map(function (g) { return "g:" + g; });

  function compareSentence(keys, start, what) {
    var a = avgDelta(keys, start), m = measure(ITEMS.total, start).delta, gap = a - m;
    var verdict = Math.abs(gap) < 1 ? "à peu près comme le marché" : (gap > 0 ? "mieux que le marché" : "moins bien que le marché");
    return what + " : " + pct(a) + " en moyenne sur la période, contre " + pct(m) + " pour l'ensemble des offres, soit " + verdict + ".";
  }
  function upWord(keys, start, what) {
    var pos = keys.filter(function (k) { return measure(ITEMS[k], start).delta > 0; }).length;
    if (pos === keys.length) return "Les " + what + " qui montent le plus : ";
    if (pos === 0) return "Aucun des " + what + " ne monte sur la période. Ceux qui résistent le mieux : ";
    return "Les " + what + " qui montent ou résistent le mieux : ";
  }
  function names(keys, start) { return keys.map(function (k) { return ITEMS[k].label + " (" + pct(measure(ITEMS[k], start).delta) + ")"; }); }

  function firstVal(item, start) { var i = lowerBound(item.xs, start); return i < item.xs.length ? item.ys[i] : 0; }
  function buildView(start) {
    var v = state.view, keys = [], summary = "", market = false, note = "", sub = "", newStart = null, show = null;
    if (v.indexOf("rome") === 0 || v === "vac") {
      // données trimestrielles : il faut au moins 2 trimestres pour comparer
      var ref = v === "vac" ? ITEMS["v:" + Object.keys(D.vacants)[0]] : ITEMS[ROME_KEYS[0]], xs0 = ref.xs, minStart = xs0[xs0.length - 2];
      if (start > minStart) { start = minStart; newStart = start; note = "Données trimestrielles : la période est étendue au minimum à 2 trimestres."; }
    }
    var tot = measure(ITEMS.total, start);
    if (v === "global") {
      keys = ["total", "newp"];
      var cur = ITEMS.total.ys[ITEMS.total.ys.length - 1];
      summary = "Sur la période, le volume d'offres d'emploi en France a évolué de " + pct(tot.delta) + ".";
      sub = "Indice actuel : " + nf1.format(cur) + " (100 = 1er février 2020). Les nouvelles offres réagissent plus vite que le stock.";
    } else if (v === "empUp" || v === "empDown") {
      keys = rank(ALL_CATS, start, 5, v === "empUp" ? "up" : "down");
      summary = (v === "empUp" ? upWord(keys, start, "métiers") : "Les métiers qui baissent le plus : ") + names(keys, start).join(", ") + ".";
      sub = "Marché global sur la même période : " + pct(tot.delta) + ". Catégories Indeed (34), pas des métiers fins.";
      market = true;
    } else if (v === "secUp" || v === "secDown") {
      keys = rank(ALL_GRPS, start, 5, v === "secUp" ? "up" : "down");
      summary = (v === "secUp" ? upWord(keys, start, "secteurs") : "Les secteurs qui baissent le plus : ") + names(keys, start).join(", ") + ".";
      sub = "Marché global sur la même période : " + pct(tot.delta) + ". Secteur = moyenne simple de ses catégories.";
      market = true;
    } else if (v === "aiExp") {
      keys = ALL_CATS.slice().sort(function (a, b) { return ITEMS[b].expo - ITEMS[a].expo; }).slice(0, 5);
      summary = compareSentence(keys, start, "Les 5 catégories jugées les plus exposées à l'IA");
      sub = "Classement par score d'exposition (estimation). La courbe montre ce que font leurs offres d'emploi.";
      market = true; note = "Les scores d'exposition sont mes estimations, pas des données officielles. À affiner.";
    } else if (v === "aiUp") {
      var exposed = ALL_CATS.filter(function (k) { return ITEMS[k].expo >= 55; });
      keys = rank(exposed, start, 5, "up");
      summary = compareSentence(keys, start, "Parmi les " + exposed.length + " catégories très exposées à l'IA, les 5 qui montent le plus");
      sub = "Exposées = score d'exposition estimé de 55 ou plus. Ce n'est pas la même chose que « métiers de l'IA », qu'Indeed ne distingue pas.";
      market = true; note = "Les scores d'exposition sont mes estimations, pas des données officielles. À affiner.";
    } else if (v === "romeUp" || v === "romeDown") {
      var big = ROME_KEYS.filter(function (k) { return firstVal(ITEMS[k], start) >= ROME_MIN; });
      keys = rank(big, start, 5, v === "romeUp" ? "up" : "down");
      summary = (v === "romeUp" ? upWord(keys, start, "métiers") : "Les métiers précis qui baissent le plus : ") + names(keys, start).join(", ") + ".";
      sub = "Offres enregistrées, cumul sur 4 trimestres glissants (toutes offres, France), parmi les " + big.length + " métiers qui avaient au moins " + ROME_MIN.toLocaleString("fr-FR") + " offres cumulées au départ. Source : France Travail ; les pics isolés sont lissés. Marché global (Indeed) sur la même période : " + pct(tot.delta) + ".";
      market = true;
    } else if (v === "romeTech") {
      keys = TECH_CODES.map(function (c) { return "t:" + state.techMetric + ":" + c; }).filter(function (k) { return ITEMS[k]; });
      show = keys.slice(0, 6); // les 6 plus gros métiers ; les autres se cochent dans la liste
      if (state.techMetric === "emb") { // les embauches sont publiées avec retard : on étend la période pour les inclure
        var ex = ITEMS["t:emb:" + TECH_CODES[0]].xs, es = ex[ex.length - 2];
        if (start > es) { start = es; newStart = es; note = "Embauches publiées jusqu'à " + qLabel(ex[ex.length - 1]) + " : la période est étendue pour les inclure."; }
      }
      var ao = techAgg("off"), ad = techAgg("dem"), ae = techAgg("emb");
      var ratioAt = function (i) {
        var k = ad.xs.indexOf(ao.xs[i]); if (k < 3) return null;
        return ao.ys[i] / ((ad.ys[k] + ad.ys[k - 1] + ad.ys[k - 2] + ad.ys[k - 3]) / 4) * 100;
      };
      var r0 = ratioAt(Math.min(lowerBound(ao.xs, start), ao.xs.length - 1)), r1 = ratioAt(ao.xs.length - 1);
      summary = "Métiers tech (" + TECH_CODES.length + ") : offres " + pct(measure(ao, start).delta) + ", demandeurs inscrits " + pct(measure(ad, start).delta) + (ae.xs.length > 1 ? ", embauches " + pct(measure(ae, Math.min(start, ae.xs[ae.xs.length - 2])).delta) + " (jusqu'à " + qLabel(ae.xs[ae.xs.length - 1]) + ")" : "") + " sur la période.";
      sub = "Offres pour 100 demandeurs inscrits : " + (r0 == null ? "n/d" : nf1.format(r0)) + " → " + nf1.format(r1) + ". Offres et embauches en cumul sur 4 trimestres glissants ; demandeurs = catégorie A (sans emploi, en recherche active). France Travail.";
      market = state.techMetric !== "ratio";
    } else if (v === "romeAi" || v === "romeAiUp") {
      var scored = ROME_KEYS.filter(function (k) { return ITEMS[k].expo != null && firstVal(ITEMS[k], start) >= ROME_MIN; });
      if (v === "romeAi") {
        var lastOf = function (k) { return ITEMS[k].ys[ITEMS[k].ys.length - 1]; };
        keys = scored.slice().sort(function (a, b) { return (ITEMS[b].expo - ITEMS[a].expo) || (lastOf(b) - lastOf(a)); }).slice(0, 5); // à égalité : le plus gros volume d'offres
        summary = compareSentence(keys, start, "Les 5 métiers précis les plus exposés à l'IA, parmi " + scored.length + " métiers de plus de " + ROME_MIN.toLocaleString("fr-FR") + " offres");
        sub = "Classement par score d'exposition estimé. La courbe suit leurs offres d'emploi (cumul sur 4 trimestres, France Travail).";
      } else {
        var expd = scored.filter(function (k) { return ITEMS[k].expo >= 60; });
        keys = rank(expd, start, 5, "up");
        summary = compareSentence(keys, start, "Parmi les " + expd.length + " métiers précis très exposés à l'IA (score estimé de 60 ou plus), les 5 dont les offres montent le plus");
        sub = "Ces métiers sont exposés, mais leurs offres ne reculent pas forcément : l'exposition n'est pas une prédiction de suppression.";
      }
      market = true; note = AI_NOTE;
    } else if (v === "romeQ") {
      var qs = buildQ();
      keys = qs.map(function (q) { return q.key; });
      var d5 = measure(ITEMS[keys[keys.length - 1]], start).delta, d1 = measure(ITEMS[keys[0]], start).delta;
      summary = "Offres d'emploi selon le niveau d'exposition à l'IA : " + pct(d5) + " pour le cinquième de métiers le plus exposé, " + pct(d1) + " pour le moins exposé (marché global Indeed : " + pct(tot.delta) + ").";
      sub = qs.n + " métiers précis répartis en 5 groupes de même effectif selon le score d'exposition estimé ; somme de leurs offres, cumul sur 4 trimestres. Pour comparaison, la Banque de France (oct. 2026) observe que les embauches en CDI des 21-29 ans ont reculé d'environ 28 % depuis novembre 2022 dans le cinquième de métiers le plus exposé, contre environ 8 % dans le moins exposé ; l'étude ne permet pas d'attribuer cet écart à l'IA (hausse des taux, correction post-Covid de la tech, télétravail).";
      market = true; note = AI_NOTE;
    } else if (v === "vac") {
      var vk = Object.keys(D.vacants).map(function (c) { return "v:" + c; });
      keys = vk.slice().sort(function (a, b) { return ITEMS[b].ys[ITEMS[b].ys.length - 1] - ITEMS[a].ys[ITEMS[a].ys.length - 1]; }).slice(0, 5);
      if (keys.indexOf("v:J") < 0 && ITEMS["v:J"]) keys.push("v:J");
      var lastQ = ITEMS[keys[0]].xs[ITEMS[keys[0]].xs.length - 1];
      summary = "Les secteurs qui peinent le plus à recruter : " + keys.filter(function (k) { return k !== "v:J" || keys.indexOf("v:J") < 5; }).slice(0, 3).map(function (k) { return ITEMS[k].label + " (" + nf1.format(ITEMS[k].ys[ITEMS[k].ys.length - 1]) + " %)"; }).join(", ") + ".";
      sub = "Taux d'emplois vacants = postes à pourvoir pour 100 postes (occupés ou vacants), entreprises de plus de 10 salariés, T" + (Math.floor(new Date(lastQ).getUTCMonth() / 3) + 1) + " " + new Date(lastQ).getUTCFullYear() + ". La tech (information & communication) est toujours affichée. Un taux élevé = recrutement difficile, ce n'est pas un volume d'offres.";
      note = (note ? note + " " : "") + "Cette vue est en niveaux (%), pas en base 100 : les courbes macro et le marché global sont donc masqués ici.";
    } else if (v === "tech") {
      keys = ALL_CATS.filter(function (k) { return ITEMS[k].tech; });
      summary = compareSentence(keys, start, "Les métiers de la tech (3 catégories)");
      sub = "Développement logiciel, systèmes et solutions IT, infrastructure et support IT.";
      market = true;
    } else {
      keys = state.mine.slice();
      if (!keys.length) { summary = "Choisis un ou plusieurs métiers à suivre dans le panneau ci-dessus."; }
      else { summary = compareSentence(keys, start, keys.length > 1 ? "Tes " + keys.length + " métiers suivis" : "Ton métier suivi"); sub = "Détail par métier dans la liste à droite."; market = true; }
    }
    return { keys: keys, summary: summary, sub: sub, market: market, note: note, start: newStart, show: show };
  }

  /* ---------- séries pour le graphique ---------- */
  function toSeries(item, start, color, dash) {
    var i0 = lowerBound(item.xs, start);
    if (i0 >= item.xs.length) return null;
    var base = state.rebase === "window" ? item.ys[i0] : item.ys[closest(item.xs, FIRST)];
    var x = item.xs.slice(i0), y = item.ys.slice(i0).map(function (v) { return item.kind === "vac" ? v : v / base * 100; });
    var m = measure(item, start);
    return { item: item, color: color, dash: dash, x: x, y: y, delta: m.delta, n: x.length };
  }

  var chart = null, cursorX = null;
  var cursorPlugin = {
    id: "cursor",
    afterDraw: function (c) {
      if (cursorX == null) return;
      var xs = c.scales.x, a = c.chartArea, px = xs.getPixelForValue(cursorX);
      if (px < a.left || px > a.right) return;
      var ctx = c.ctx; ctx.save(); ctx.strokeStyle = getComputedStyle(document.body).getPropertyValue("--muted"); ctx.globalAlpha = .5; ctx.setLineDash([3, 3]);
      ctx.beginPath(); ctx.moveTo(px, a.top); ctx.lineTo(px, a.bottom); ctx.stroke(); ctx.restore();
    }
  };
  function tickDates(scale) {
    var min = scale.min, max = scale.max, span = (max - min) / DAY, t = [];
    if (span <= 70) { for (var d = min; d <= max; d += 7 * DAY) t.push({ value: d }); }
    else {
      var step = Math.max(1, Math.ceil(span / 30.4 / 8)), d0 = new Date(min);
      var y = d0.getUTCFullYear(), m = d0.getUTCMonth() + 1;
      if (step > 1) m = m - (m % step); // alignement sur des mois « ronds »
      for (var cur = Date.UTC(y, m, 1); cur <= max; m += step, cur = Date.UTC(y, m, 1)) if (cur >= min) t.push({ value: cur });
    }
    scale.ticks = t;
  }

  function render() {
    var start = startMs();
    var vw = buildView(start);
    if (vw.start) start = vw.start;
    $("#summary").innerHTML = "";
    var s = document.createElement("span"); s.textContent = vw.summary; $("#summary").appendChild(s);
    if (vw.sub) { var sm = document.createElement("small"); sm.textContent = vw.sub; $("#summary").appendChild(sm); }
    var notes = $("#notes"); notes.innerHTML = "";
    function addNote(t) { var n = document.createElement("div"); n.className = "note"; n.textContent = t; notes.appendChild(n); }
    if (vw.note) addNote(vw.note);

    var series = [];
    vw.keys.forEach(function (k, i) { var sr = toSeries(ITEMS[k], start, PALETTE[i % PALETTE.length], null); if (sr) series.push(sr); });
    var isVac = state.view === "vac";
    if (vw.market && !isVac) { var mk = toSeries(ITEMS.total, start, MARKET_COL, [2, 3]); if (mk) { mk.item = Object.assign({}, ITEMS.total, { label: "Marché (toutes offres)" }); series.push(mk); } }
    (isVac ? [] : state.macro).forEach(function (k) {
      var it = ITEMS["m:" + k]; if (!it) return;
      var sr = toSeries(it, start, it.color, [7, 4]);
      if (!sr) { addNote(it.label + " : aucune donnée sur cette période."); return; }
      if (sr.n < 2) addNote(it.label + " : un seul point sur cette période (" + it.sub + "). Allonge la période pour voir une courbe.");
      series.push(sr);
    });

    var datasets = series.map(function (sr) {
      var isMacro = sr.item.kind === "macro";
      return {
        label: sr.item.label, data: sr.x.map(function (t, i) { return { x: t, y: sr.y[i] }; }), parsing: false,
        borderColor: sr.color, backgroundColor: sr.color, borderWidth: isMacro ? 2.2 : (sr.dash ? 1.8 : 2.4), borderDash: sr.dash || [],
        hidden: !!(vw.show && sr.item.kind === "rome" && vw.show.indexOf(sr.item.key) < 0),
        pointRadius: (isMacro || sr.item.kind === "rome") ? 3 : 0, pointHoverRadius: 0, tension: 0, spanGaps: true
      };
    });
    var cs = getComputedStyle(document.body), grid = cs.getPropertyValue("--grid"), ink = cs.getPropertyValue("--muted");
    if (chart) chart.destroy();
    chart = new Chart($("#chart"), {
      type: "line", data: { datasets: datasets }, plugins: [cursorPlugin],
      options: {
        responsive: true, maintainAspectRatio: false, animation: false, normalized: true,
        events: [],
        plugins: { legend: { display: false }, tooltip: { enabled: false } },
        scales: {
          x: { type: "linear", min: start, max: LAST, grid: { color: grid }, ticks: { color: ink, maxRotation: 0, callback: function (v) { var span = (LAST - start) / DAY; return span <= 70 ? dateFr(v, true).replace(/ \d{4}$/, "") : dateFr(v, false); } }, afterBuildTicks: tickDates },
          y: { grace: "6%", grid: { color: grid }, ticks: { color: ink }, title: { display: true, color: ink, text: isVac ? "Taux d'emplois vacants (%)" : (state.rebase === "window" ? "Indice (100 = début de la période)" : "Indice (100 = février 2020)") } }
        }
      }
    });
    chart._series = series;
    renderList(series, vw);
    renderTech(start);
    $("#listtitle").textContent = (VIEWS.filter(function (v) { return v.id === state.view; })[0] || {}).title;
    cursorX = null; $("#readout").textContent = "Survole le graphique pour lire les valeurs à une date.";
    save();
  }

  /* tableau de bord tech : une ligne par métier, variations sur la période choisie */
  function renderTech(start) {
    var on = state.view === "romeTech" && TECH_CODES.length > 0;
    $("#techrow").hidden = !on; $("#techcard").hidden = !on;
    if (!on) return;
    var tb = $("#techtable"); tb.innerHTML = "";
    var cols = [["off", "Offres (4 trim.)"], ["dem", "Demandeurs inscrits"], ["emb", "Embauches (4 trim.)"], ["ratio", "Offres / 100 demandeurs"]];
    var thead = document.createElement("thead"), hr = document.createElement("tr");
    ["Métier"].concat(AIR ? ["Exposition IA (estim.)"] : [], cols.map(function (c) { return c[1]; }), ["Tension (1 à 5)"]).forEach(function (h) { var th = document.createElement("th"); th.textContent = h; hr.appendChild(th); });
    thead.appendChild(hr); tb.appendChild(thead);
    var tbody = document.createElement("tbody");
    TECH_CODES.forEach(function (c) {
      var tr = document.createElement("tr"), td = document.createElement("td");
      td.textContent = ITEMS["t:off:" + c].label; tr.appendChild(td);
      if (AIR) { var te = document.createElement("td"); te.textContent = AIR[c] != null ? AIR[c] + " / 100" : "–"; tr.appendChild(te); }
      cols.forEach(function (col) {
        var it = ITEMS["t:" + col[0] + ":" + c], cell = document.createElement("td");
        var m = it && measure(it, Math.min(start, it.xs[it.xs.length - 2])); // série qui s'arrête avant la période : on prend ses 2 derniers points
        if (!m) { cell.textContent = "–"; }
        else {
          var b = document.createElement("b"); b.textContent = (col[0] === "ratio" ? nf1 : nf0).format(it.ys[it.ys.length - 1]);
          var d = document.createElement("span"); d.className = "dl " + ((m.delta >= 0) !== !!it.invert ? "up" : "down"); d.textContent = " " + (m.delta >= 0 ? "▲" : "▼") + " " + pct(m.delta);
          cell.appendChild(b); cell.appendChild(d);
        }
        tr.appendChild(cell);
      });
      var t = TECHD.items[c].t || {}, yrs = Object.keys(t).sort(), tc = document.createElement("td");
      if (yrs.length) {
        var ly = yrs[yrs.length - 1], py = yrs.length > 1 ? yrs[yrs.length - 2] : null;
        tc.textContent = t[ly] + " / 5 (" + ly + ")" + (py && t[py] !== t[ly] ? ", " + t[py] + " en " + py : "");
      } else tc.textContent = "–";
      tr.appendChild(tc); tbody.appendChild(tr);
    });
    tb.appendChild(tbody);
    var e = ITEMS["t:emb:" + TECH_CODES[0]], dm = ITEMS["t:dem:" + TECH_CODES[0]];
    $("#technote").textContent = "Variations sur la période choisie (2 trimestres au minimum). Demandeurs : stock à fin " + qLabel(dm.xs[dm.xs.length - 1]) + (e ? " ; embauches publiées jusqu'à " + qLabel(e.xs[e.xs.length - 1]) : "") + ". Couleurs : vert = favorable à un candidat (plus d'offres ou d'embauches, moins de demandeurs). Tension : 5 = recrutement très difficile pour les employeurs (plutôt favorable aux candidats), 1 = facile. Indicateur annuel de France Travail.";
  }

  function renderList(series, vw) {
    var ul = $("#list"); ul.innerHTML = "";
    series.forEach(function (sr, i) {
      var li = document.createElement("li");
      var sw = document.createElement("span"); sw.className = "sw" + (sr.dash ? " dash" : ""); sw.style.background = sr.color; sw.style.borderColor = sr.color;
      var nm = document.createElement("span"); nm.className = "nm"; nm.textContent = sr.item.label;
      var bits = [];
      if (sr.item.expo != null && sr.item.kind !== "tot") bits.push("Exposition IA (estim.) : " + Math.round(sr.item.expo) + "/100");
      if (sr.item.kind === "vac") bits.push("Dernier taux : " + nf1.format(sr.item.ys[sr.item.ys.length - 1]) + " %");
      if (sr.item.kind === "rome") {
        var lastV = sr.item.ys[sr.item.ys.length - 1];
        bits.push(sr.item.unitLabel ? nf1.format(lastV).replace(/,0$/, "") + " " + sr.item.unitLabel : sr.item.sub + " · " + sr.n + " trim.");
      }
      if (sr.item.kind === "macro") bits.push(sr.item.sub + ", " + sr.n + " point" + (sr.n > 1 ? "s" : ""));
      if (sr.item.sub && sr.item.kind === "grp") bits.push(sr.item.sub);
      if (bits.length) { var sm = document.createElement("small"); sm.textContent = bits.join(" · "); nm.appendChild(sm); }
      var dl = document.createElement("span"); dl.className = "dl " + ((sr.delta >= 0) !== !!sr.item.invert ? "up" : "down");
      dl.textContent = (sr.delta >= 0 ? "▲ " : "▼ ") + (isPts(sr.item) ? nfPct.format(sr.delta).replace("%", "") + " pt" : pct(sr.delta));
      dl.title = isPts(sr.item) ? "Variation du taux, en points de pourcentage" : "Variation sur la période";
      if (!chart.isDatasetVisible(i)) li.classList.add("off");
      li.appendChild(sw); li.appendChild(nm); li.appendChild(dl);
      li.addEventListener("click", function () {
        var vis = chart.isDatasetVisible(i); chart.setDatasetVisibility(i, !vis); li.classList.toggle("off", vis); chart.update("none");
      });
      ul.appendChild(li);
    });
  }

  /* ---------- lecture au survol ---------- */
  var canvas = $("#chart");
  function hover(e) {
    if (!chart) return;
    var r = canvas.getBoundingClientRect(), px = (e.touches ? e.touches[0].clientX : e.clientX) - r.left;
    var t = chart.scales.x.getValueForPixel(px);
    if (t < chart.scales.x.min || t > chart.scales.x.max) return;
    cursorX = t; chart.draw();
    var ro = $("#readout"); ro.innerHTML = "";
    var h = document.createElement("span"); h.innerHTML = "<b></b>"; h.firstChild.textContent = dateFr(t, true); ro.appendChild(h);
    chart._series.forEach(function (sr, i) {
      if (!chart.isDatasetVisible(i)) return;
      var j = lowerBound(sr.x, t + 1) - 1; if (j < 0) return;
      var sp = document.createElement("span"); sp.style.color = sr.color;
      sp.textContent = sr.item.label + " " + nf1.format(sr.y[j]) + (sr.item.kind === "macro" ? " (relevé " + dateFr(sr.x[j], false) + ")" : "");
      ro.appendChild(sp);
    });
  }
  canvas.addEventListener("mousemove", hover);
  canvas.addEventListener("touchmove", hover, { passive: true });
  canvas.addEventListener("mouseleave", function () { cursorX = null; if (chart) chart.draw(); });

  /* ---------- contrôles ---------- */
  /* navigation à deux niveaux : thème > vue, avec un interrupteur de détail pour les vues qui existent en deux versions */
  var NAV = [
    { g: "Emploi", items: [
      { label: "Global", id: "global" },
      { label: "Métiers en montée", ids: { cat: "empUp", rome: "romeUp" } },
      { label: "Métiers en descente", ids: { cat: "empDown", rome: "romeDown" } },
      { label: "Secteurs en montée", id: "secUp" },
      { label: "Secteurs en descente", id: "secDown" },
      { label: "Difficultés de recrutement", id: "vac" }] },
    { g: "Exposition à l'IA", items: [
      { label: "Les plus exposés", ids: { cat: "aiExp", rome: "romeAi" } },
      { label: "Exposés et en montée", ids: { cat: "aiUp", rome: "romeAiUp" } },
      { label: "Par niveau d'exposition", id: "romeQ" }] },
    { g: "Tech", items: [
      { label: "Vue rapide (Indeed)", id: "tech" },
      { label: "Tableau de bord (France Travail)", id: "romeTech" }] },
    { g: "Mon suivi", items: [{ label: "Mes métiers", id: "mine" }] }
  ];
  function hasView(id) { return VIEWS.some(function (v) { return v.id === id; }); }
  function itemIds(it) { return (it.id ? [it.id] : [it.ids.cat, it.ids.rome]).filter(hasView); }
  function resolveItem(it, detail) { return it.id || (detail === "rome" && hasView(it.ids.rome) ? it.ids.rome : it.ids.cat); }
  function renderNav() {
    var groups = NAV.map(function (g) { return { g: g.g, items: g.items.filter(function (it) { return itemIds(it).length; }) }; }).filter(function (g) { return g.items.length; });
    var cur = null, curG = groups[0];
    groups.forEach(function (g) { g.items.forEach(function (it) { if (itemIds(it).indexOf(state.view) >= 0) { cur = it; curG = g; } }); });
    var detail = cur && cur.ids && state.view === cur.ids.rome ? "rome" : "cat";
    var gEl = $("#groups"); gEl.innerHTML = "";
    groups.forEach(function (g) {
      var b = chip(g.g, g === curG, function () { state.view = resolveItem(g.items[0], detail); buildControls(); render(); });
      b.classList.add("tab"); gEl.appendChild(b);
    });
    var vEl = $("#views"); vEl.innerHTML = "";
    curG.items.forEach(function (it) { vEl.appendChild(chip(it.label, it === cur, function () { state.view = resolveItem(it, detail); buildControls(); render(); })); });
    var both = cur && cur.ids && hasView(cur.ids.cat) && hasView(cur.ids.rome);
    $("#detailrow").hidden = !both;
    var dEl = $("#detail"); dEl.innerHTML = "";
    if (both) [["cat", "Catégories Indeed (quotidien)"], ["rome", "Métiers précis (trimestriel)"]].forEach(function (d) {
      dEl.appendChild(chip(d[1], detail === d[0], function () { state.view = cur.ids[d[0]]; buildControls(); render(); }));
    });
  }
  function chip(label, pressed, onClick) {
    var b = document.createElement("button"); b.className = "chip"; b.type = "button"; b.textContent = label; b.setAttribute("aria-pressed", pressed ? "true" : "false"); b.addEventListener("click", onClick); return b;
  }
  function buildControls() {
    renderNav();
    var pEl = $("#periods"); pEl.innerHTML = "";
    PERIODS.forEach(function (p) { pEl.appendChild(chip(p[0], !state.custom && state.period === p[1], function () { state.period = p[1]; state.custom = ""; $("#custom").value = ""; buildControls(); render(); })); });
    var mEl = $("#macros"); mEl.innerHTML = "";
    Object.keys(D.macro).forEach(function (k) {
      var lab = document.createElement("label"); lab.className = "chk";
      var cb = document.createElement("input"); cb.type = "checkbox"; cb.checked = state.macro.indexOf(k) >= 0;
      cb.addEventListener("change", function () { state.macro = cb.checked ? state.macro.concat(k) : state.macro.filter(function (x) { return x !== k; }); render(); });
      var dot = document.createElement("span"); dot.className = "sw dash"; dot.style.borderColor = MACRO_COL[k]; dot.style.width = "16px";
      lab.appendChild(cb); lab.appendChild(dot); lab.appendChild(document.createTextNode(D.macro[k].label)); mEl.appendChild(lab);
    });
    var tm = $("#techmetrics"); tm.innerHTML = "";
    Object.keys(TECH_METRICS).forEach(function (k) { tm.appendChild(chip(TECH_METRICS[k], state.techMetric === k, function () { state.techMetric = k; buildControls(); render(); })); });
    $("#minebox").hidden = state.view !== "mine";
    var mine = $("#mine"); mine.innerHTML = "";
    cats.slice().sort(function (a, b) { return ITEMS["c:" + a].label.localeCompare(ITEMS["c:" + b].label, "fr"); }).forEach(function (k) {
      var key = "c:" + k, on = state.mine.indexOf(key) >= 0;
      var lab = document.createElement("label"); lab.className = "chk";
      var cb = document.createElement("input"); cb.type = "checkbox"; cb.checked = on; cb.disabled = !on && state.mine.length >= 6;
      cb.addEventListener("change", function () { state.mine = cb.checked ? state.mine.concat(key) : state.mine.filter(function (x) { return x !== key; }); buildControls(); render(); });
      lab.appendChild(cb); lab.appendChild(document.createTextNode(ITEMS[key].label)); mine.appendChild(lab);
    });
    // métiers précis (ROME) choisis : cases à décocher
    state.mine.filter(function (k) { return k.indexOf("r:") === 0; }).forEach(function (key) {
      var lab = document.createElement("label"); lab.className = "chk";
      var cb = document.createElement("input"); cb.type = "checkbox"; cb.checked = true;
      cb.addEventListener("change", function () { state.mine = state.mine.filter(function (x) { return x !== key; }); buildControls(); render(); });
      lab.appendChild(cb); lab.appendChild(document.createTextNode(ITEMS[key].label + " (précis)")); mine.appendChild(lab);
    });
    var ra = $("#romeadd"); ra.innerHTML = "";
    if (ROME_KEYS.length) {
      var dl = $("#romelist");
      if (!dl.children.length) Object.keys(ROME_BY_LABEL).sort().forEach(function (l) { var o = document.createElement("option"); o.value = l; dl.appendChild(o); });
      var inp = document.createElement("input"); inp.setAttribute("list", "romelist"); inp.type = "search"; inp.placeholder = "Ajouter un métier précis (ex. Développeur, Comptable…)"; inp.style.cssText = "width:min(420px,100%);font:inherit;color:inherit;background:var(--card);border:1px solid var(--line);border-radius:8px;padding:6px 10px";
      inp.setAttribute("aria-label", "Ajouter un métier précis");
      inp.addEventListener("change", function () {
        var k = ROME_BY_LABEL[inp.value];
        if (k && state.mine.indexOf(k) < 0 && state.mine.length < 6) { state.mine = state.mine.concat(k); buildControls(); render(); }
        else inp.value = "";
      });
      ra.appendChild(inp);
    }
  }
  $("#rebase").value = state.rebase;
  $("#rebase").addEventListener("change", function (e) { state.rebase = e.target.value; render(); });
  $("#custom").max = new Date(LAST - 7 * DAY).toISOString().slice(0, 10);
  $("#custom").value = state.custom;
  $("#custom").addEventListener("change", function (e) { state.custom = e.target.value; buildControls(); render(); });
  $("#updated").textContent = "Données Indeed jusqu'au " + dateFr(LAST, true) + ". Collecte du " + dateFr(ms(D.updated), true) + " (relancer fetch_data.py pour actualiser).";
  try {
    var collectes = " Dernières collectes : Indeed/Eurostat/DARES " + dateFr(ms(D.updated), true);
    if (window.ROME && window.ROME.updated) collectes += ", France Travail " + dateFr(ms(window.ROME.updated), true);
    $("#updated").textContent += collectes + ".";
  } catch (e) { /* ROME absent ou date illisible : on garde le texte de base */ }
  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", render);

  // sur téléphone, les options secondaires sont repliées (sauf si une courbe macro est déjà choisie)
  if (window.matchMedia("(max-width:860px)").matches) {
    if (!state.macro.length) $("#opts").open = false;
    $("#minebox details").open = false; // la liste de 34 métiers occuperait tout l'écran
  }
  buildControls();
  render();
})();
