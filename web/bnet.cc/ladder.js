// bnet.cc ladder pages: renders the standings the BNET Command Center server pushes
// (ladder-push.php keeps them, ladder-data.php serves them) into #ladder.
//
// Every view is a plain link (?g=sc&p=2 …), so pages can be bookmarked and shared, and the back
// button works; clicks are handled here without reloading. Names are only ever set as text.
(function () {
  'use strict';

  var PAGE_SIZE = 50;
  var GAMES = [
    { key: 'sc', product: 'STAR', label: 'StarCraft' },
    { key: 'bw', product: 'SEXP', label: 'Brood War' },
    { key: 'w2', product: 'W2BN', label: 'Warcraft II' },
    { key: 'd2', label: 'Diablo II' },
    { key: 'w3', label: 'WarCraft III', href: 'war3-ladder.php' }
  ];
  var D2_CLASSES = ['Amazon', 'Sorceress', 'Necromancer', 'Paladin', 'Barbarian', 'Druid', 'Assassin'];

  var root = document.getElementById('ladder');
  if (!root) {
    return;
  }
  var data = null;

  // ---- small DOM helpers ------------------------------------------------------------------

  function el(tag, attrs, children) {
    var node = document.createElement(tag);
    if (attrs) {
      Object.keys(attrs).forEach(function (k) {
        if (k === 'text') {
          node.textContent = attrs[k];
        } else if (k === 'className') {
          node.className = attrs[k];
        } else {
          node.setAttribute(k, attrs[k]);
        }
      });
    }
    (children || []).forEach(function (c) {
      if (c !== null && c !== undefined) {
        node.appendChild(typeof c === 'string' ? document.createTextNode(c) : c);
      }
    });
    return node;
  }

  function number(n) {
    return Number(n || 0).toLocaleString('en-US');
  }

  function day(secs) {
    if (!secs) {
      return '—';
    }
    return new Date(secs * 1000).toLocaleDateString('en-US', { month: 'short', day: 'numeric', year: 'numeric' });
  }

  function ago(secs) {
    var d = Math.max(0, Math.floor(Date.now() / 1000) - secs);
    if (d < 90) return 'just now';
    if (d < 5400) return Math.round(d / 60) + ' minutes ago';
    if (d < 129600) return Math.round(d / 3600) + ' hours ago';
    return Math.round(d / 86400) + ' days ago';
  }

  // ---- the view in the address bar ----------------------------------------------------------

  function readState() {
    var q = new URLSearchParams(window.location.search);
    var s = {
      g: q.get('g') || 'sc',
      l: q.get('l') === 'ironman' ? 'ironman' : 'ladder',
      s: ['wins', 'games'].indexOf(q.get('s')) >= 0 ? q.get('s') : 'rank',
      p: Math.max(1, parseInt(q.get('p'), 10) || 1),
      q: (q.get('q') || '').trim(),
      m: q.get('m') === 'hardcore' ? 'hardcore' : 'softcore',
      e: q.get('e') === 'classic' ? 'classic' : 'expansion',
      c: (q.get('c') || 'all').toLowerCase()
    };
    if (!GAMES.some(function (g) { return g.key === s.g && !g.href; })) {
      s.g = 'sc';
    }
    return s;
  }

  // A link to the current view with some of it changed. Changing anything but the page goes back
  // to page 1 and drops a finished search.
  function link(state, changes) {
    var next = Object.assign({}, state, changes);
    if (!('p' in changes)) {
      next.p = 1;
    }
    if (!('q' in changes)) {
      next.q = '';
    }
    var q = new URLSearchParams();
    q.set('g', next.g);
    if (next.g === 'w2' && next.l === 'ironman') q.set('l', 'ironman');
    if (next.g === 'd2') {
      if (next.m !== 'softcore') q.set('m', next.m);
      if (next.e !== 'expansion') q.set('e', next.e);
      if (next.c !== 'all') q.set('c', next.c);
    } else if (next.s !== 'rank') {
      q.set('s', next.s);
    }
    if (next.q) q.set('q', next.q);
    if (next.p > 1) q.set('p', String(next.p));
    return '?' + q.toString();
  }

  function go(href) {
    window.history.pushState(null, '', href);
    render();
    root.scrollIntoView({ block: 'start' });
  }

  root.addEventListener('click', function (event) {
    var a = event.target.closest ? event.target.closest('a[data-view]') : null;
    if (a && !event.metaKey && !event.ctrlKey && !event.shiftKey && event.button === 0) {
      event.preventDefault();
      go(a.getAttribute('href'));
    }
  });
  window.addEventListener('popstate', render);

  // ---- pieces shared by every ladder ----------------------------------------------------------

  function viewLink(state, changes, label, current) {
    if (current) {
      return el('span', { className: 'ladder-current', text: label });
    }
    return el('a', { href: link(state, changes), 'data-view': '1', text: label });
  }

  function choices(state, label, options) {
    var items = [el('span', { className: 'ladder-label', text: label + ':' })];
    options.forEach(function (o, i) {
      if (i > 0) items.push(el('span', { className: 'ladder-sep', text: '|' }));
      items.push(viewLink(state, o.changes, o.label, o.current));
    });
    return el('div', { className: 'ladder-choices' }, items);
  }

  function gameTabs(state) {
    var tabs = GAMES.map(function (g) {
      if (g.href) {
        return el('a', { className: 'ladder-tab', href: g.href, text: g.label });
      }
      if (g.key === state.g) {
        return el('span', { className: 'ladder-tab ladder-tab-on', text: g.label });
      }
      return el('a', { className: 'ladder-tab', href: link(state, { g: g.key, s: 'rank' }), 'data-view': '1', text: g.label });
    });
    return el('div', { className: 'ladder-tabs' }, tabs);
  }

  function searchForm(state, placeholder) {
    var input = el('input', { type: 'text', name: 'q', maxlength: '32', placeholder: placeholder, 'aria-label': placeholder });
    input.value = state.q;
    var form = el('form', { className: 'ladder-search' }, [input, el('button', { type: 'submit', text: 'Find' })]);
    form.addEventListener('submit', function (event) {
      event.preventDefault();
      go(link(state, { q: input.value.trim(), p: 1 }));
    });
    return form;
  }

  function pager(state, total) {
    var pages = Math.max(1, Math.ceil(total / PAGE_SIZE));
    var page = Math.min(state.p, pages);
    return el('div', { className: 'ladder-pager' }, [
      page > 1 ? el('a', { href: link(state, { p: page - 1, q: state.q }), 'data-view': '1', text: '« Previous' }) : el('span', { className: 'ladder-dim', text: '« Previous' }),
      el('span', { text: 'Page ' + page + ' of ' + pages }),
      page < pages ? el('a', { href: link(state, { p: page + 1, q: state.q }), 'data-view': '1', text: 'Next »' }) : el('span', { className: 'ladder-dim', text: 'Next »' })
    ]);
  }

  function table(headers, rows) {
    var head = el('tr', null, headers.map(function (h) {
      return el('td', { className: 'ladder-th' + (h.num ? ' ladder-num' : ''), text: h.label });
    }));
    var t = el('table', { className: 'ladder-table', cellspacing: '1', cellpadding: '0' }, [el('tbody', null, [head].concat(rows))]);
    return el('div', { className: 'ladder-scroll' }, [t]);
  }

  // Which page to show: the one holding the searched name if there is a search, else the asked one.
  function pageOf(state, list, nameOf) {
    var found = -1;
    if (state.q) {
      var wanted = state.q.toLowerCase();
      found = list.findIndex(function (x) { return nameOf(x).toLowerCase() === wanted; });
      if (found < 0) {
        found = list.findIndex(function (x) { return nameOf(x).toLowerCase().indexOf(wanted) === 0; });
      }
    }
    var pages = Math.max(1, Math.ceil(list.length / PAGE_SIZE));
    var page = found >= 0 ? Math.floor(found / PAGE_SIZE) + 1 : Math.min(state.p, pages);
    return { page: page, found: found };
  }

  // ---- StarCraft, Brood War, Warcraft II -------------------------------------------------------

  function renderRated(state, game) {
    var entry = data.games.filter(function (g) { return g.product === game.product; })[0];
    var parts = [];
    var leagues = entry ? entry.leagues : [];
    var hasIron = leagues.some(function (l) { return l.league === 'ironman'; });
    var league = leagues.filter(function (l) { return l.league === (hasIron ? state.l : 'ladder'); })[0] || { players: [] };
    var title = game.label + (state.l === 'ironman' && hasIron ? ' Iron Man Ladder' : ' Ladder');
    document.title = title + ' - bnet.cc';
    parts.push(el('b', { className: 'header', text: title }));
    parts.push(el('p', { className: 'ladder-note' }, [
      'The top ' + data.max_rank + ' players by rating. A player joins the ladder after ' + data.ladder_min_wins +
        ' normal-game wins; a game counts when it lasts longer than ' + Math.round(data.min_game_seconds / 60) + ' minutes.'
    ]));
    if (hasIron) {
      parts.push(choices(state, 'Ladder', [
        { label: 'Standard', changes: { l: 'ladder' }, current: state.l !== 'ironman' },
        { label: 'Iron Man', changes: { l: 'ironman' }, current: state.l === 'ironman' }
      ]));
    }
    parts.push(choices(state, 'Order by', [
      { label: 'Rank', changes: { s: 'rank' }, current: state.s === 'rank' },
      { label: 'Most wins', changes: { s: 'wins' }, current: state.s === 'wins' },
      { label: 'Most games', changes: { s: 'games' }, current: state.s === 'games' }
    ]));
    parts.push(searchForm(state, 'Player name'));

    var games = function (p) { return p.wins + p.losses + p.disconnects; };
    var list = league.players.slice();
    if (state.s === 'wins') {
      list.sort(function (a, b) { return b.wins - a.wins || a.rank - b.rank; });
    } else if (state.s === 'games') {
      list.sort(function (a, b) { return games(b) - games(a) || a.rank - b.rank; });
    }
    if (!list.length) {
      parts.push(el('p', { className: 'ladder-empty', text: 'No one is ranked yet.' }));
      return parts;
    }
    var where = pageOf(state, list, function (p) { return p.name; });
    if (state.q && where.found < 0) {
      parts.push(el('p', { className: 'ladder-miss', text: 'No ranked player named “' + state.q + '”.' }));
    }
    var start = (where.page - 1) * PAGE_SIZE;
    var rows = list.slice(start, start + PAGE_SIZE).map(function (p, i) {
      var cls = ((start + i) % 2 ? 'ladder-alt' : 'ladder-row') + (start + i === where.found ? ' ladder-found' : '');
      var total = games(p);
      return el('tr', { className: cls }, [
        el('td', { className: 'ladder-num', text: String(p.rank) }),
        el('td', { className: 'ladder-name', text: p.name }),
        el('td', { className: 'ladder-num ladder-rating', text: number(p.rating) }),
        el('td', { className: 'ladder-num', text: number(p.wins) }),
        el('td', { className: 'ladder-num', text: number(p.losses) }),
        el('td', { className: 'ladder-num', text: number(p.disconnects) }),
        el('td', { className: 'ladder-num', text: total ? Math.round((100 * p.wins) / total) + '%' : '—' }),
        el('td', { className: 'ladder-num', text: number(p.high_rating) }),
        el('td', { className: 'ladder-num ladder-date', text: day(p.last_game) })
      ]);
    });
    parts.push(table([
      { label: 'Rank', num: true }, { label: 'Player' }, { label: 'Rating', num: true }, { label: 'W', num: true },
      { label: 'L', num: true }, { label: 'D', num: true }, { label: 'Win %', num: true }, { label: 'Best', num: true },
      { label: 'Last game', num: true }
    ], rows));
    parts.push(pager(Object.assign({}, state, { p: where.page }), list.length));
    return parts;
  }

  // ---- Diablo II -------------------------------------------------------------------------------

  function renderDiablo(state) {
    var hardcore = state.m === 'hardcore';
    var expansion = state.e === 'expansion';
    var classes = expansion ? D2_CLASSES : D2_CLASSES.slice(0, 5);
    var klass = classes.filter(function (c) { return c.toLowerCase() === state.c; })[0] || null;
    var season = data.diablo2.season;
    var title = 'Diablo II ' + (expansion ? 'Expansion ' : 'Classic ') + (hardcore ? 'Hardcore' : 'Softcore') + ' Ladder';
    document.title = title + ' - bnet.cc';
    var parts = [el('b', { className: 'header', text: title })];
    parts.push(el('p', { className: 'ladder-note' }, [
      el('span', { className: 'ladder-season', text: 'Season ' + season.number }),
      ' began ' + day(season.started) + '. Characters rank by experience, down to rank ' + data.max_rank +
        '; a new season moves every ladder character to the non-ladder realm.'
    ]));
    parts.push(choices(state, 'Mode', [
      { label: 'Softcore', changes: { m: 'softcore' }, current: !hardcore },
      { label: 'Hardcore', changes: { m: 'hardcore' }, current: hardcore }
    ]));
    parts.push(choices(state, 'Game', [
      { label: 'Lord of Destruction', changes: { e: 'expansion' }, current: expansion },
      { label: 'Classic', changes: { e: 'classic', c: ['druid', 'assassin'].indexOf(state.c) >= 0 ? 'all' : state.c }, current: !expansion }
    ]));
    parts.push(choices(state, 'Class', [{ label: 'All', changes: { c: 'all' }, current: !klass }].concat(classes.map(function (c) {
      return { label: c, changes: { c: c.toLowerCase() }, current: klass === c };
    }))));
    parts.push(searchForm(state, 'Character name'));

    var list = data.diablo2.characters.filter(function (ch) {
      return ch.hardcore === hardcore && ch.expansion === expansion && (klass ? ch.class === klass && ch.class_rank : ch.rank);
    });
    if (!list.length) {
      parts.push(el('p', { className: 'ladder-empty', text: 'No ' + (klass ? klass : 'character') + ' is on this ladder yet.' }));
      return parts;
    }
    var where = pageOf(state, list, function (ch) { return ch.name; });
    if (state.q && where.found < 0) {
      parts.push(el('p', { className: 'ladder-miss', text: 'No character named “' + state.q + '” on this ladder.' }));
    }
    var start = (where.page - 1) * PAGE_SIZE;
    var rows = list.slice(start, start + PAGE_SIZE).map(function (ch, i) {
      var cls = ((start + i) % 2 ? 'ladder-alt' : 'ladder-row') + (start + i === where.found ? ' ladder-found' : '');
      var name = el('td', { className: 'ladder-name' }, [ch.name]);
      if (ch.dead) {
        name.appendChild(el('span', { className: 'ladder-dead', text: ' (dead)' }));
      }
      return el('tr', { className: cls }, [
        el('td', { className: 'ladder-num', text: String(klass ? ch.class_rank : ch.rank) }),
        name,
        el('td', { text: ch.class }),
        el('td', { className: 'ladder-num', text: String(ch.level) }),
        el('td', { className: 'ladder-num', text: number(ch.experience) })
      ]);
    });
    parts.push(table([
      { label: 'Rank', num: true }, { label: 'Character' }, { label: 'Class' }, { label: 'Level', num: true }, { label: 'Experience', num: true }
    ], rows));
    parts.push(pager(Object.assign({}, state, { p: where.page }), list.length));
    return parts;
  }

  // ---- the page ----------------------------------------------------------------------------------

  function render() {
    var state = readState();
    root.textContent = '';
    root.appendChild(gameTabs(state));
    if (!data) {
      return;
    }
    var game = GAMES.filter(function (g) { return g.key === state.g; })[0];
    var parts = state.g === 'd2' ? renderDiablo(state) : renderRated(state, game);
    parts.forEach(function (p) { root.appendChild(p); });
    root.appendChild(el('p', { className: 'ladder-stamp', text: 'Standings from ' + data.server_name + ', ' + ago(data.generated) + '.' }));
  }

  function fail(message) {
    render();
    root.appendChild(el('p', { className: 'ladder-empty', text: message }));
  }

  render();
  root.appendChild(el('p', { className: 'ladder-empty', text: 'Loading the ladder…' }));
  fetch(root.getAttribute('data-src') || 'ladder-data.php', { cache: 'no-cache' })
    .then(function (r) {
      if (r.status === 404) {
        throw new Error('The ladder has not been published yet. Check back soon.');
      }
      if (!r.ok) {
        throw new Error('The ladder could not be loaded (' + r.status + '). Try again in a minute.');
      }
      return r.json();
    })
    .then(function (json) {
      if (!json || !json.games || !json.diablo2) {
        throw new Error('The ladder could not be read. Try again in a minute.');
      }
      data = json;
      render();
    })
    .catch(function (e) {
      fail(e && e.message ? e.message : 'The ladder could not be loaded.');
    });
})();
