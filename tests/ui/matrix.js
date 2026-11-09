// Parcours automatisé de l'interface : à coller dans la console du navigateur
// (page index.html servie en HTTP). Pour chaque thème -> vue -> niveau de détail
// -> indicateur tech -> période, clique, attend 60 ms, puis vérifie :
// aucune erreur JS, liste #list li non vide, pas de débordement horizontal.
// Retourne {combos, nbErreurs, nbListesVides, nbDebordements, echecs}.
(async function () {
  var erreurs = [];
  window.addEventListener("error", function (e) { erreurs.push(String(e.message)); });
  window.addEventListener("unhandledrejection", function (e) { erreurs.push("promesse : " + String(e.reason)); });
  var pause = function (ms) { return new Promise(function (r) { setTimeout(r, ms); }); };
  var res = { combos: 0, nbErreurs: 0, nbListesVides: 0, nbDebordements: 0, echecs: [] };

  // Les boutons sont reconstruits à chaque rendu : on relit toujours le DOM.
  function visible(el) { return !!el && el.offsetParent !== null; }
  function textes(sel) {
    var c = document.querySelector(sel);
    if (!c || !visible(c)) return [];
    return Array.prototype.map.call(c.querySelectorAll("button"), function (b) { return b.textContent; });
  }
  async function presser(sel, texte) {
    var c = document.querySelector(sel);
    var b = c && Array.prototype.filter.call(c.querySelectorAll("button"), function (x) { return x.textContent === texte; })[0];
    if (!b) return false;
    b.click();
    await pause(60);
    return true;
  }
  function verifier(chemin) {
    res.combos++;
    var avant = res.nbErreurs;
    if (erreurs.length) { res.nbErreurs += erreurs.length; erreurs = []; }
    var ok = res.nbErreurs === avant;
    if (document.querySelectorAll("#list li").length === 0) { res.nbListesVides++; ok = false; }
    if (document.documentElement.scrollWidth > window.innerWidth) { res.nbDebordements++; ok = false; }
    if (!ok && res.echecs.length < 10) res.echecs.push(chemin.join(" > "));
  }

  var themes = textes("#groups");
  for (var i = 0; i < themes.length; i++) {
    await presser("#groups", themes[i]);
    var vues = textes("#views");
    for (var j = 0; j < vues.length; j++) {
      await presser("#views", vues[j]);
      var details = textes("#detail"); if (!details.length) details = [null];
      for (var k = 0; k < details.length; k++) {
        if (details[k] !== null) await presser("#detail", details[k]);
        var metriques = textes("#techmetrics"); if (!metriques.length) metriques = [null];
        for (var m = 0; m < metriques.length; m++) {
          if (metriques[m] !== null) await presser("#techmetrics", metriques[m]);
          var periodes = textes("#periods"); if (!periodes.length) periodes = [null];
          for (var p = 0; p < periodes.length; p++) {
            if (periodes[p] !== null) await presser("#periods", periodes[p]); else await pause(60);
            verifier([themes[i], vues[j], details[k], metriques[m], periodes[p]].filter(function (x) { return x !== null; }));
          }
        }
      }
    }
  }
  return res;
})();
