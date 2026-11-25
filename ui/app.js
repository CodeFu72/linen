// Linen: write, fix, save. Saved files autosave as you type; new notes are kept in a
// private session file until saved, so nothing is lost if the window closes.
(function () {
  'use strict';
  var $ = function (id) { return document.getElementById(id); };
  var page = $('page');
  var doc = { path: '', name: '', mtime: 0, dirty: false };
  // Tabs: each holds its note's state and words. The page shows the current one.
  var tabs = [], cur = 0;
  var state = { settings: {}, home: '', projects_dir: '' };
  var savedRange = null, clickPoint = { x: 200, y: 200 }, aiJob = null;
  // A locked note's password lives only here, in memory, while it's open.
  var secret = '';

  // ---- Server -------------------------------------------------------------------
  function api(path, body) {
    return fetch('/api/' + path, {
      method: body === undefined ? 'GET' : 'POST',
      headers: { 'X-Linen-Token': window.LINEN_TOKEN, 'Content-Type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body)
    }).then(function (r) { return r.json().then(function (j) { if (!r.ok) throw new Error(j.error || 'Something went wrong.'); return j; }); });
  }

  // ---- Text -------------------------------------------------------------------------
  function getText() { var t = page.innerText; return t === '\n' ? '' : t; }
  function setText(t) { page.textContent = t; }
  function count(t) { var w = (t.match(/\S+/g) || []).length; return w + (w === 1 ? ' word' : ' words') + ' · ' + t.length.toLocaleString() + ' characters'; }
  function drawCount() { $('count').textContent = count(getText()); }
  // Typing only schedules the word count and tab name, a quarter second after the last key (review fix #9).
  function scheduleCount() {
    clearTimeout(scheduleCount.timer);
    scheduleCount.timer = setTimeout(function () {
      drawCount();
      var on = $('tabs') && $('tabs').querySelector('.tab.on .tab-name');
      if (on && !doc.name) on.textContent = (doc.locked ? '🔒 ' : '') + (firstLine() === 'Untitled' ? 'Untitled' : firstLine().slice(0, 28));
    }, 250);
  }
  function firstLine() { return (getText().split('\n').find(function (l) { return l.trim(); }) || 'Untitled').replace(/^#+\s*/, '').trim().slice(0, 60); }
  // A first line used as a file name: no slashes, which would turn into folders.
  function fileStem() { return firstLine().replace(/[\/\\]/g, '-').trim() || 'Untitled'; }
  // Shows a path with ~ for the home folder, only when it really starts there (review fix #17).
  function tilde(p) { return state.home && p.indexOf(state.home + '/') === 0 ? '~' + p.slice(state.home.length) : p; }

  function toast(msg, ms) {
    var t = $('toast'); t.textContent = msg; t.hidden = false;
    clearTimeout(toast.timer); toast.timer = setTimeout(function () { t.hidden = true; }, ms || 2600);
  }
  function clock(d) { return d.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' }); }

  // ---- Document state -----------------------------------------------------------------
  function drawName() {
    var n = $('doc-name'); n.textContent = (doc.locked ? '🔒 ' : '') + (doc.name || 'Untitled'); n.classList.toggle('dirty', doc.dirty);
    document.title = (doc.dirty ? '• ' : '') + (doc.name || 'Untitled') + ' — Linen';
    var lb = $('lock-btn'); lb.textContent = doc.locked ? '🔒 Locked' : '🔒 Lock'; lb.classList.toggle('on', !!doc.locked);
    lb.title = doc.locked ? 'Locked: it saves with your password. Click to change the password.' : 'Save this note password-protected (Ctrl+L)';
    drawTabs();
  }

  // ---- Tabs -------------------------------------------------------------------------------
  var desk = document.querySelector('.desk');
  function stash() {
    if (cur < 0 || cur >= tabs.length) return;
    tabs[cur] = { doc: doc, secret: secret, text: getText(), scroll: desk.scrollTop };
  }
  function blankTab() { return { doc: { path: '', name: '', mtime: 0, dirty: false }, secret: '', text: '', scroll: 0 }; }
  function isBlank(t) { return !t.doc.path && !t.doc.dirty && !t.text.trim(); }
  function show(i, quietSave) {
    if (i === cur && tabs.length) return;
    // Save the note being left right away, so nothing waits on a timer meant for it.
    clearTimeout(markDirty.timer);
    if (cur >= 0 && doc.path && doc.dirty) save(true);
    stash();
    cur = Math.max(0, Math.min(i, tabs.length - 1));
    var t = tabs[cur];
    doc = t.doc; secret = t.secret; setText(t.text);
    drawName(); drawCount(); setSaved(doc.path ? (doc.dirty ? 'Not saved yet' : 'Saved') : (t.text.trim() ? 'Not saved yet · kept safe until you save' : 'New note'));
    desk.scrollTop = t.scroll || 0; page.focus(); keepSession();
  }
  // Where a file should open: its own tab if it's already open, the current tab if that
  // one is an empty new note, otherwise a fresh tab. Returns true if it switched to an open tab.
  function prepareTab(f) {
    stash();
    var at = f && f.path ? tabs.findIndex(function (t) { return t.doc.path === f.path; }) : -1;
    if (at >= 0) { show(at); return true; }
    if (!tabs.length || !isBlank(tabs[cur])) { tabs.push(blankTab()); show(tabs.length - 1); }
    return false;
  }
  // Saves one tab (current or background) and reports whether it is safely on disk.
  // Resolves 'saved', 'conflict' or 'failed'; never throws (review fix #4).
  function saveTab(t) {
    if (t.doc.saving) return t.doc.saving.then(function () { return saveTab(t); });
    if (!t.doc.path || !t.doc.dirty) return Promise.resolve('saved');
    var body = { path: t.doc.path, text: t.text, mtime: t.doc.mtime };
    if (t.doc.locked) body.password = t.secret;
    return api('save', body).then(function (r) {
      if (r.conflict) return 'conflict';
      t.doc.mtime = r.mtime; t.doc.dirty = false; return 'saved';
    }).catch(function () { return 'failed'; });
  }
  function closeTab(i, force) {
    stash();
    var t = tabs[i];
    if (!force && !t.doc.path && t.text.trim()) { askClose(i); return; }
    // A saved file with unsaved changes closes only after its save lands. If the save
    // fails or the file changed on disk, the tab stays open and says so (review fix #4).
    if (!force && t.doc.path && t.doc.dirty) {
      if (i === cur) clearTimeout(markDirty.timer);
      saveTab(t).then(function (result) {
        var j = tabs.indexOf(t);
        if (j < 0) return;
        if (result === 'saved') { closeTab(j, true); return; }
        if (j !== cur) show(j);
        drawName();
        if (result === 'conflict') showConflict();
        else { setSaved('Not saved'); toast('Couldn’t save ' + tabName(t) + ', so the tab stays open.', 5000); }
      });
      return;
    }
    tabs.splice(i, 1);
    if (!tabs.length) tabs.push(blankTab());
    var next = Math.min(i <= cur ? Math.max(0, cur - 1) : cur, tabs.length - 1);
    if (i === cur) { cur = -1; show(next); } else { cur = i < cur ? cur - 1 : cur; drawTabs(); keepSession(); }
    if (t.doc.name) toast('Closed ' + t.doc.name);
  }
  // Closing a note that was never saved: Linen's own box, not the browser's pop-up.
  var closeAfterSave = null;
  function askClose(i) {
    var t = tabs[i];
    $('ask-title').textContent = '“' + tabName(t) + '” isn’t saved';
    $('ask-modal').hidden = false; $('ask-save').focus();
    $('ask-save').onclick = function () { closeAsk(); if (i !== cur) show(i); closeAfterSave = doc; openProjectDialog(); };
    $('ask-drop').onclick = function () { closeAsk(); var at = tabs.indexOf(t); if (at >= 0) closeTab(at, true); };
    $('ask-keep').onclick = function () { closeAsk(); if (i !== cur) show(i); };
  }
  function closeAsk() { $('ask-modal').hidden = true; page.focus(); }
  function tabName(t) {
    if (t.doc.name) return t.doc.name;
    var line = (t.text.split('\n').find(function (l) { return l.trim(); }) || '').replace(/^#+\s*/, '').trim();
    return line ? line.slice(0, 28) : 'Untitled';
  }
  function drawTabs() {
    var bar = $('tabs'); if (!bar) return;
    stash();
    bar.replaceChildren();
    tabs.forEach(function (t, i) {
      var b = document.createElement('div');
      b.className = 'tab' + (i === cur ? ' on' : '') + (t.doc.dirty ? ' dirty' : '');
      b.title = t.doc.path ? tilde(t.doc.path) : 'Not saved yet';
      var label = document.createElement('span'); label.className = 'tab-name';
      label.textContent = (t.doc.locked ? '🔒 ' : '') + tabName(t);
      var x = document.createElement('button'); x.className = 'tab-x'; x.type = 'button'; x.textContent = '×'; x.title = 'Close tab (Ctrl+W)';
      x.addEventListener('click', function (ev) { ev.stopPropagation(); closeTab(i); });
      b.addEventListener('click', function () { show(i); });
      b.addEventListener('auxclick', function (ev) { if (ev.button === 1) closeTab(i); });
      b.append(label, x); bar.append(b);
    });
    var add = document.createElement('button'); add.className = 'tab-add'; add.type = 'button'; add.textContent = '＋'; add.title = 'New tab (Ctrl+T)';
    add.addEventListener('click', function () { commands.new(); });
    bar.append(add);
  }
  function setSaved(text) { $('saved').textContent = text; }
  function loadDoc(f) {
    // A locked file arrives without its text: ask for the password first.
    if (f.locked && typeof f.text !== 'string') { askPassword('unlock', f); return; }
    doc = { path: f.path || '', name: f.name || '', mtime: f.mtime || 0, dirty: false, locked: !!f.locked };
    if (!doc.locked) secret = '';
    setText(f.text || ''); drawName(); drawCount(); setSaved(doc.path ? 'Opened ' + clock(new Date()) : 'New note');
    keepSession(); page.focus();
  }
  function markDirty() {
    // Each key used to rebuild the tab bar and recount the whole note twice; now only a
    // change from saved to unsaved redraws, and the count waits for a pause (review fix #9).
    var was = doc.dirty;
    doc.dirty = true;
    if (!was) drawName();
    scheduleCount();
    setSaved(doc.path ? 'Saving soon…' : 'Not saved yet · kept safe until you save');
    clearTimeout(markDirty.timer);
    // Locked notes re-encrypt on every save, which works the processor, so they wait longer.
    markDirty.timer = setTimeout(function () { if (doc.path) save(true); else keepSession(); }, doc.locked ? 5000 : 1200);
  }

  // The session keeps the open file and any unsaved words, privately, for next launch.
  function keepSession() {
    // Locked notes never go in the session at all: not their words, not even their name
    // or where they live (review fix #7). They stay shut after a restart.
    stash();
    var open = tabs.filter(function (t) { return !t.doc.locked; });
    var list = open.map(function (t) {
      return { path: t.doc.path, name: t.doc.name, mtime: t.doc.mtime, dirty: t.doc.dirty,
        text: t.doc.dirty || !t.doc.path ? t.text : '' };
    });
    var at = open.indexOf(tabs[cur]), c = list[at] || {};
    return api('session', { tabs: list, current: Math.max(0, at), path: c.path || '', name: c.name || '', mtime: c.mtime || 0,
      dirty: !!c.dirty, text: c.text || '' }).then(function () { keepSession.warned = false; }).catch(function (e) {
      // Say so once, instead of claiming unsaved words are safe (review fix #11).
      if (!keepSession.warned) { keepSession.warned = true; toast('Couldn’t keep the safety copy of unsaved notes: ' + e.message, 6000); }
    });
  }
  function withPw(body) { if (doc.locked) body.password = secret; return body; }

  function save(quiet) {
    if (!doc.path) { openProjectDialog(); return Promise.resolve(); }
    var d = doc;
    // One save at a time per note, so a Ctrl+S during an autosave can't look like an
    // outside change (review fix #11). The second request runs right after the first.
    if (d.saving) { d.again = true; return d.saving; }
    var text = getText();
    d.saving = api('save', withPw({ path: d.path, text: text, mtime: d.mtime })).then(function (r) {
      if (r.conflict) { if (d === doc) showConflict(); return; }
      d.mtime = r.mtime; d.dirty = d === doc ? getText() !== text : false; drawName();
      if (d !== doc) { keepSession(); return; }
      setSaved((quiet ? 'Autosaved ' : 'Saved ') + clock(new Date()));
      if (!quiet) toast('Saved ' + doc.name);
      keepSession();
    }).catch(function (e) { setSaved('Not saved: ' + e.message); }).then(function () {
      d.saving = null;
      if (d.again) { d.again = false; if (d === doc && d.dirty) return save(true); }
    });
    return d.saving;
  }
  function showConflict() {
    var b = $('banner'); b.replaceChildren(document.createTextNode(doc.name + ' changed on disk since you opened it.'));
    var keep = document.createElement('button'); keep.textContent = 'Keep my version';
    keep.onclick = function () { api('save', withPw({ path: doc.path, text: getText(), force: true })).then(function (r) { doc.mtime = r.mtime; doc.dirty = false; drawName(); b.hidden = true; setSaved('Saved ' + clock(new Date())); }); };
    var load = document.createElement('button'); load.textContent = 'Load theirs';
    load.onclick = function () { api('open_path', { path: doc.path }).then(function (f) { loadDoc(f); b.hidden = true; }); };
    b.append(keep, load); b.hidden = false;
  }
  // `d` is the note the save started from; if another tab is showing now, that note is
  // updated in its own tab, never the one on screen (review fix #5).
  function saved(r, d) {
    if (!r || r.cancelled) { closeAfterSave = null; return; }
    d = d || doc;
    d.path = r.path; d.name = r.name; d.mtime = r.mtime; d.dirty = false;
    drawName(); toast('Saved to ' + tilde(r.path)); keepSession();
    if (d === doc) setSaved('Saved ' + clock(new Date()));
    if (closeAfterSave === d) {
      closeAfterSave = null;
      var at = tabs.findIndex(function (t) { return t.doc === d; });
      if (at >= 0) closeTab(at, true);
    }
  }

  // ---- Commands ---------------------------------------------------------------------
  var commands = {
    new: function () { stash(); if (isBlank(tabs[cur])) { page.focus(); return; } tabs.push(blankTab()); show(tabs.length - 1); setSaved('New note'); },
    close: function () {
      if (tabs.length === 1 && !doc.path && !getText().trim()) { toast('Nothing open to close.'); return; }
      closeTab(cur);
    },
    nextTab: function (step) { if (tabs.length > 1) show((cur + step + tabs.length) % tabs.length); },
    open: function () {
      var dir = doc.path ? doc.path.replace(/\/[^/]*$/, '') : state.projects_dir;
      api('open_dialog', { dir: dir }).then(function (r) { openFiles(r.files || []); }).catch(function (e) { toast(e.message, 6000); });
    },
    save: function () { save(false); },
    saveAs: function () {
      closeModal();
      if (blockCopy()) return;
      var dir = doc.path ? doc.path.replace(/\/[^/]*$/, '') : state.projects_dir, d = doc;
      api('save_as', { dir: dir, name: doc.name || fileStem() + '.txt', text: getText() }).then(function (r) { saved(r, d); }).catch(function (e) { toast(e.message, 4000); });
    },
    saveProject: function () { if (blockCopy()) return; openProjectDialog(); },
    lock: function () {
      if (!getText().trim()) { toast('Nothing to lock yet.'); return; }
      askPassword('lock');
    },
    saveVault: function () { if (blockCopy()) return; api('save_vault', { text: getText(), title: firstLine() }).then(function (r) { if (!r.cancelled) toast('Saved to Obsidian: ' + r.name); }).catch(function (e) { toast(e.message, 4000); }); },
    more: function () { $('more').hidden = !$('more').hidden; },
    // Omarchy (follows the running theme, when there is one) → Dusk → Dawn → …
    theme: function () {
      var order = state.omarchy_theme ? ['omarchy', 'dusk', 'dawn'] : ['dusk', 'dawn'];
      setSetting('theme', order[(order.indexOf(themeName()) + 1) % order.length]);
    },
    font: function () { var order = ['atkinson', 'serif', 'mono'], cur = state.settings.font || 'atkinson'; setSetting('font', order[(order.indexOf(cur) + 1) % order.length]); },
    bigger: function () { setSetting('size', Math.min(30, (state.settings.size || 19) + 1)); },
    smaller: function () { setSetting('size', Math.max(13, (state.settings.size || 19) - 1)); },
    password: function () { api('password', { length: 20 }).then(function (r) { copyText(r.password); toast('A new strong password is on your clipboard.'); }); }
  };
  // Copies elsewhere would be unlocked, so a locked note only saves to its own file.
  function blockCopy() {
    if (doc.locked) toast('This note is locked, so it only saves to its locked file. Ctrl+N starts a new note.', 4500);
    return !!doc.locked;
  }

  // ---- 🔒 Lock / unlock ---------------------------------------------------------------
  var pwMode = '', pwFile = null;
  function askPassword(mode, f) {
    pwMode = mode; pwFile = f || null;
    var lock = mode === 'lock';
    $('pw-title').textContent = lock ? (doc.locked ? 'Change the password' : 'Lock this note') : 'Unlock ' + f.name;
    $('pw-about').textContent = !lock ? 'This note is locked. Type its password to open it.'
      : (doc.locked ? 'It saves with the new password from now on.'
        : doc.path ? 'It saves as ' + doc.name + '.gpg, and the unlocked copy is removed.' : 'Next, pick where to save it.')
        + ' If the password is forgotten, nobody can open it, not even Linen.';
    $('pw2-row').hidden = !lock;
    $('pw-ok').textContent = lock ? '🔒 Lock' : 'Unlock';
    $('pw1').value = ''; $('pw2').value = ''; $('pw-show').checked = false; showPw();
    $('pw-error').hidden = true; $('pw-modal').hidden = false; $('pw1').focus();
  }
  function showPw() { var t = $('pw-show').checked ? 'text' : 'password'; $('pw1').type = t; $('pw2').type = t; }
  function pwError(msg) { var e = $('pw-error'); e.textContent = msg; e.hidden = false; }
  function closePw() { $('pw-modal').hidden = true; $('pw1').value = ''; $('pw2').value = ''; page.focus(); }
  $('pw-show').addEventListener('change', showPw);
  $('pw-cancel').addEventListener('click', function () {
    var wasUnlock = pwMode === 'unlock'; closePw();
    if (wasUnlock && tabs.length > 1 && isBlank({ doc: doc, text: getText() })) closeTab(cur);
  });
  $('pw-form').addEventListener('submit', function (ev) {
    ev.preventDefault();
    var pw = $('pw1').value, ok = $('pw-ok');
    if (!pw) { pwError('Type a password.'); return; }
    if (ok.disabled) return;
    if (pwMode === 'unlock') {
      ok.disabled = true;
      api('unlock', { path: pwFile.path, password: pw }).then(function (r) {
        secret = pw; closePw(); loadDoc(r);
      }).catch(function (e) { pwError(e.message); $('pw1').select(); }).finally(function () { ok.disabled = false; });
      return;
    }
    if (pw.length < 8) { pwError('Use at least 8 characters.'); return; }
    if (pw !== $('pw2').value) { pwError('The two passwords don’t match.'); return; }
    var text = getText(), before = doc;
    ok.disabled = true;
    api('lock', { path: doc.path, text: text, password: pw, dir: state.projects_dir, name: doc.name || fileStem() + '.txt' }).then(function (r) {
      if (r.cancelled) return;
      // If another note was opened while the save box was up, the locked file holds
      // what was locked; never tie it to the note now on screen.
      if (doc !== before) { closePw(); toast('Locked ' + r.name + '.', 4000); return; }
      var was = doc.locked;
      secret = pw; closePw();
      doc = { path: r.path, name: r.name, mtime: r.mtime, dirty: getText() !== text, locked: true }; drawName();
      setSaved('Locked ' + clock(new Date()));
      toast(was ? 'Password changed.' : r.removed ? 'Locked. The unlocked copy was removed.' : 'Locked: ' + tilde(r.path), 4000);
      keepSession();
    }).catch(function (e) { pwError(e.message); }).finally(function () { ok.disabled = false; });
  });

  function setSetting(k, v) { state.settings[k] = v; applySettings(); var o = {}; o[k] = v; api('settings', o).catch(function () {}); }
  // The chosen theme: the saved choice, else Omarchy's colors when there are any.
  function themeName() {
    var t = state.settings.theme;
    if (t === 'omarchy' && !state.omarchy_theme) return 'dusk';
    return t === 'dusk' || t === 'dawn' || t === 'omarchy' ? t : (state.omarchy_theme ? 'omarchy' : 'dusk');
  }
  var THEME_VARS = [];
  function applyTheme() {
    var root = document.documentElement, name = themeName(), t = state.omarchy_theme;
    THEME_VARS.forEach(function (k) { root.style.removeProperty(k); });
    THEME_VARS = [];
    if (name === 'omarchy' && t) {
      root.dataset.theme = t.mode === 'light' ? 'dawn' : 'dusk';
      Object.keys(t.vars).forEach(function (k) { root.style.setProperty(k, t.vars[k]); THEME_VARS.push(k); });
    } else root.dataset.theme = name;
    $('theme-name').textContent = { omarchy: 'Omarchy', dusk: 'Dusk', dawn: 'Dawn' }[name];
  }
  // Called by the window whenever Omarchy switches themes (no polling).
  window.linenTheme = function (t) { state.omarchy_theme = t; applyTheme(); };
  function applySettings() {
    var s = state.settings, root = document.documentElement;
    applyTheme();
    root.dataset.font = s.font || 'atkinson';
    root.style.setProperty('--text-size', (s.size || 19) + 'px');
    $('font-name').textContent = { atkinson: 'Atkinson', serif: 'Serif', mono: 'Mono' }[s.font || 'atkinson'];
  }
  document.querySelectorAll('[data-cmd]').forEach(function (b) {
    b.addEventListener('click', function (ev) { ev.stopPropagation(); var c = b.dataset.cmd; if (c !== 'more') $('more').hidden = true; commands[c](); });
  });
  document.addEventListener('click', function (ev) { if (!$('more').contains(ev.target)) $('more').hidden = true; if (!$('syn').contains(ev.target)) $('syn').hidden = true; });

  // ---- Save to project ---------------------------------------------------------------
  var projects = [], chosen = '', projectFor = null;
  function openProjectDialog() {
    api('projects').then(function (r) {
      // No project folders yet: a normal Save box instead of an empty list (review fix #8).
      if (!r.projects.length) { commands.saveAs(); return; }
      projects = r.projects; chosen = chosen && projects.indexOf(chosen) >= 0 ? chosen : (state.settings.last_project || projects[0] || '');
      $('proj-filter').value = ''; $('proj-title').value = firstLine();
      $('proj-ext').value = state.settings.last_ext || 'txt';
      drawProjects(); $('modal').hidden = false; $('proj-filter').focus();
      projectFor = doc;
    }).catch(function () { commands.saveAs(); });
  }
  function drawProjects() {
    var q = $('proj-filter').value.trim().toLowerCase(), list = $('proj-list');
    list.replaceChildren();
    var shown = projects.filter(function (p) { return !q || p.toLowerCase().indexOf(q) >= 0; });
    if (shown.length && shown.indexOf(chosen) < 0) chosen = shown[0];
    shown.forEach(function (p) {
      var b = document.createElement('button'); b.type = 'button'; b.textContent = p; b.setAttribute('role', 'option'); b.setAttribute('aria-selected', String(p === chosen));
      b.onclick = function () {
        chosen = p;
        list.querySelectorAll('[role=option]').forEach(function (o) { o.setAttribute('aria-selected', String(o === b)); });
        $('proj-where').textContent = 'Saves to ' + tilde(state.projects_dir + '/' + chosen + '/');
      };
      b.ondblclick = saveToProject;
      list.append(b);
    });
    $('proj-where').textContent = chosen ? 'Saves to ' + tilde(state.projects_dir + '/' + chosen + '/') : 'No project matches.';
  }
  function saveToProject() {
    if (!chosen) return;
    var ext = $('proj-ext').value, d = projectFor || doc;
    api('save_project', { project: chosen, title: $('proj-title').value, ext: ext, text: getText() }).then(function (r) {
      closeModal(); saved(r, d); state.settings.last_project = chosen; state.settings.last_ext = ext;
      api('settings', { last_project: chosen, last_ext: ext }).catch(function () {});
    }).catch(function (e) { toast(e.message, 4000); });
  }
  function closeModal() { $('modal').hidden = true; page.focus(); }
  $('proj-filter').addEventListener('input', drawProjects);
  $('proj-filter').addEventListener('keydown', function (ev) { if (ev.key === 'Enter') { ev.preventDefault(); $('proj-title').focus(); $('proj-title').select(); } });
  $('proj-title').addEventListener('keydown', function (ev) { if (ev.key === 'Enter') saveToProject(); });
  $('proj-save').addEventListener('click', saveToProject);
  $('proj-cancel').addEventListener('click', function () { closeAfterSave = null; closeModal(); });

  // ---- Right-click helpers --------------------------------------------------------
  // The page remembers what was selected when the menu opened; each helper works on
  // that selection, or the whole note when nothing was selected.
  page.addEventListener('contextmenu', function (ev) {
    clickPoint = { x: ev.clientX, y: ev.clientY };
    var sel = window.getSelection();
    savedRange = sel.rangeCount ? sel.getRangeAt(0).cloneRange() : null;
  });
  function restore() {
    var sel = window.getSelection(); page.focus();
    if (savedRange && page.contains(savedRange.commonAncestorContainer)) { sel.removeAllRanges(); sel.addRange(savedRange); }
    return sel;
  }
  function target() {
    var sel = restore(), text = sel.toString();
    if (!text) { var r = document.createRange(); r.selectNodeContents(page); sel.removeAllRanges(); sel.addRange(r); text = getText(); }
    return text;
  }
  // insertText keeps Ctrl+Z working.
  function replaceSelection(text) { document.execCommand('insertText', false, text); markDirty(); }
  // Inserts at the caret, adding a space if it would otherwise touch a word.
  function insertAtCaret(text, inline) {
    var sel = restore(); if (sel.rangeCount) sel.collapseToEnd();
    if (inline && sel.rangeCount) {
      var r = sel.getRangeAt(0), before = '';
      if (r.startContainer.nodeType === 3 && r.startOffset > 0) before = r.startContainer.data[r.startOffset - 1];
      else { var pre = document.createRange(); pre.selectNodeContents(page); pre.setEnd(r.startContainer, r.startOffset); before = pre.toString().slice(-1); }
      if (before && !/\s/.test(before)) text = ' ' + text;
    }
    document.execCommand('insertText', false, text); markDirty();
  }
  function copyText(t) { (navigator.clipboard ? navigator.clipboard.writeText(t) : Promise.reject()).catch(function () { var a = document.createElement('textarea'); a.value = t; document.body.append(a); a.select(); document.execCommand('copy'); a.remove(); }); }

  function plain(t) {
    return t.replace(/^#{1,6}\s+/gm, '').replace(/^\s*[-*+]\s+\[[ xX]\]\s+/gm, '').replace(/^>\s?/gm, '')
      .replace(/!\[([^\]]*)\]\([^)]*\)/g, '$1').replace(/\[([^\]]+)\]\([^)]*\)/g, '$1')
      .replace(/(\*\*|__)(.+?)\1/g, '$2').replace(/(\*|_)(.+?)\1/g, '$2').replace(/`([^`]+)`/g, '$1');
  }

  var DAYS = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];
  var MONTHS = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];
  function longDate(d) { return DAYS[d.getDay()] + ', ' + MONTHS[d.getMonth()] + ' ' + d.getDate() + ', ' + d.getFullYear(); }

  var CLEANUPS = ['smart_clean', 'remove_extra_spaces', 'remove_empty_lines', 'join_paragraphs', 'quotes_straight', 'remove_duplicate_lines', 'sort_lines', 'upper_case', 'lower_case', 'title_case', 'sentence_case'];
  window.linen = {
    menu: function (id) {
      if (CLEANUPS.indexOf(id) >= 0) {
        var text = target(), d = doc, before = getText();
        api('clean', { op: id, text: text }).then(function (r) {
          if (!sameNote(d, before)) return;
          if (r.text !== text) replaceSelection(r.text); else toast('Nothing to change.');
        }).catch(function (e) { toast(e.message, 4000); });
      } else if (/^ai_/.test(id)) runAi(id, target());
      else if (id === 'synonyms') synonyms();
      else if (id === 'insert_date') insertAtCaret(longDate(new Date()), true);
      else if (id === 'insert_time') insertAtCaret(clock(new Date()), true);
      else if (id === 'insert_datetime') insertAtCaret(longDate(new Date()) + ', ' + clock(new Date()), true);
      else if (id === 'insert_checkbox') insertAtCaret('- [ ] ');
      else if (id === 'insert_divider') insertAtCaret('\n────────────\n');
      else if (id === 'insert_password') api('password', { length: 20 }).then(function (r) { insertAtCaret(r.password, true); toast('Strong password added. Save to keep it.'); });
      else if (id === 'copy_plain') { var sel = restore().toString(); copyText(plain(sel || getText())); toast('Copied as plain text.'); }
      else if (id === 'count') { var s = restore().toString(); toast((s ? 'Selection: ' : 'Whole note: ') + count(s || getText()), 3500); }
      else if (id === 'save_project') commands.saveProject();
      else if (id === 'save_vault') commands.saveVault();
    }
  };

  // Offline AI (Ollama on this laptop): nothing leaves the computer.
  var AI_LABEL = { ai_grammar: 'Fixing spelling & grammar', ai_shorter: 'Making it shorter', ai_friendly: 'Making it friendly', ai_professional: 'Making it professional',
    ai_bullets: 'Turning it into bullets', ai_summary: 'Summarizing', ai_obsidian: 'Making an Obsidian note', ai_synonyms: 'Finding synonyms' };
  // A helper's result only goes into the note it started on, and only if that note
  // wasn't changed meanwhile; otherwise nothing is replaced and it says why (review fix #5).
  function sameNote(d, before) {
    if (d !== doc) { toast('You switched notes, so the result wasn’t put in.', 4000); return false; }
    if (getText() !== before) { toast('The note changed while it worked, so nothing was replaced.', 4000); return false; }
    return true;
  }
  function runAi(tool, text, onDone) {
    if (aiJob) { toast('One thing at a time. Stop the current one first.'); return; }
    if (!text.trim()) { toast('Nothing to work on yet.'); return; }
    var range = window.getSelection().rangeCount ? window.getSelection().getRangeAt(0).cloneRange() : null;
    var d = doc, before = getText();
    $('busy').hidden = false; $('busy-text').textContent = AI_LABEL[tool] + '…';
    api('ai/start', { tool: tool, text: text }).then(function (r) {
      aiJob = r.id;
      (function poll() {
        api('ai/poll', { id: r.id }).then(function (p) {
          if (aiJob !== r.id) return;
          if (p.status && !p.done) $('busy-text').textContent = AI_LABEL[tool] + '… ' + (p.text ? '' : p.status);
          if (!p.done) { setTimeout(poll, 350); return; }
          aiJob = null; $('busy').hidden = true;
          if (p.error) { toast(p.error, 6000); return; }
          if (onDone) { onDone(p.text, d); return; }
          if (!sameNote(d, before)) return;
          var sel = window.getSelection(); page.focus();
          if (range) { sel.removeAllRanges(); sel.addRange(range); }
          if (p.text && p.text !== text) { replaceSelection(p.text); toast('Done. Ctrl+Z puts it back.', 3500); } else toast('It already looks right.');
        }).catch(function (e) { aiJob = null; $('busy').hidden = true; toast(e.message, 5000); });
      })();
    }).catch(function (e) { $('busy').hidden = true; toast(e.message, 5000); });
  }
  $('busy-stop').addEventListener('click', function () { if (aiJob) api('ai/cancel', { id: aiJob }); aiJob = null; $('busy').hidden = true; toast('Stopped.'); });

  // Synonyms for the selected word, or the word under the right-click.
  function wordRange() {
    var sel = restore(), picked = sel.toString().trim();
    if (picked && !/\s/.test(picked)) return sel.getRangeAt(0).cloneRange();
    var r = document.caretRangeFromPoint ? document.caretRangeFromPoint(clickPoint.x, clickPoint.y) : null;
    if (!r || r.startContainer.nodeType !== 3) return null;
    var s = r.startContainer.data, a = r.startOffset, b = r.startOffset, isW = function (c) { return /[\p{L}\p{N}'’-]/u.test(c); };
    while (a > 0 && isW(s[a - 1])) a--;
    while (b < s.length && isW(s[b])) b++;
    if (a === b) return null;
    var w = document.createRange(); w.setStart(r.startContainer, a); w.setEnd(r.startContainer, b); return w;
  }
  function synonyms() {
    var r = wordRange();
    if (!r) { toast('Right-click a word, or select one, for synonyms.'); return; }
    var word = r.toString().replace(/^['’-]+|['’-]+$/g, ''), node = r.startContainer.data || '';
    var around = node.slice(Math.max(0, r.startOffset - 160), r.endOffset + 160).replace(/\s+/g, ' ');
    var box = $('syn'); box.replaceChildren(); var h = document.createElement('h3'); h.textContent = 'Synonyms for “' + word + '”'; box.append(h);
    var chips = document.createElement('div'); chips.className = 'chips'; chips.textContent = 'Thinking…'; box.append(chips);
    box.style.left = Math.max(8, Math.min(clickPoint.x, window.innerWidth - 380)) + 'px'; box.style.top = Math.min(clickPoint.y + 12, window.innerHeight - 160) + 'px'; box.hidden = false;
    runAi('ai_synonyms', 'WORD: ' + word + '\nSENTENCE: ' + around, function (text, d) {
      chips.replaceChildren();
      var list = text.split(/[,\n]/).map(function (x) { return x.replace(/^[\s\d.)*-]+|[\s.]+$/g, ''); }).filter(function (x) { return x && x.toLowerCase() !== word.toLowerCase() && x.length < 40; }).slice(0, 8);
      if (!list.length) { chips.textContent = 'No good synonyms found.'; return; }
      list.forEach(function (w) {
        var b = document.createElement('button'); b.type = 'button';
        var keepCase = word[0] === word[0].toUpperCase() ? w[0].toUpperCase() + w.slice(1) : w;
        b.textContent = keepCase;
        b.onclick = function () {
          box.hidden = true;
          if (d !== doc || !page.contains(r.startContainer)) { toast('You switched notes, so the word wasn’t replaced.', 4000); return; }
          var sel = window.getSelection(); page.focus(); sel.removeAllRanges(); sel.addRange(r); replaceSelection(keepCase);
        };
        chips.append(b);
      });
    });
  }

  // ---- Typing, keys, startup ------------------------------------------------------------
  page.addEventListener('input', markDirty);
  // Copy and cut put plain text only on the clipboard: no colors or background from the page.
  function plainClip(ev, cut) {
    var t = window.getSelection().toString();
    if (!t) return;
    ev.preventDefault();
    ev.clipboardData.setData('text/plain', t);
    if (cut) { document.execCommand('delete'); markDirty(); }
  }
  page.addEventListener('copy', function (ev) { plainClip(ev, false); });
  page.addEventListener('cut', function (ev) { plainClip(ev, true); });
  // Paste stays plain text, even from web pages.
  page.addEventListener('paste', function (ev) { ev.preventDefault(); document.execCommand('insertText', false, (ev.clipboardData || window.clipboardData).getData('text/plain')); });
  document.addEventListener('keydown', function (ev) {
    var k = ev.key.toLowerCase();
    if (ev.key === 'Escape') { $('more').hidden = true; $('syn').hidden = true; if (!$('modal').hidden) { closeAfterSave = null; closeModal(); } if (!$('pw-modal').hidden) $('pw-cancel').click(); if (!$('ask-modal').hidden) closeAsk(); return; }
    if (!(ev.ctrlKey || ev.metaKey)) return;
    if (!$('ask-modal').hidden) return;
    if (!$('pw-modal').hidden) return; // No switching notes under an open password box.
    if (!$('modal').hidden) return; // ...or under the Save to project box (review fix #5).
    if (ev.key === 'Tab') { ev.preventDefault(); commands.nextTab(ev.shiftKey ? -1 : 1); return; }
    if (ev.key === 'PageDown' || ev.key === 'PageUp') { ev.preventDefault(); commands.nextTab(ev.key === 'PageDown' ? 1 : -1); return; }
    if (/^[1-9]$/.test(ev.key) && !ev.shiftKey) { ev.preventDefault(); show(Math.min(+ev.key, tabs.length) - 1); return; }
    if (k === 't') { ev.preventDefault(); commands.new(); return; }
    if (k === 's' && ev.shiftKey) { ev.preventDefault(); commands.saveAs(); }
    else if (k === 's') { ev.preventDefault(); commands.save(); }
    else if (k === 'o') { ev.preventDefault(); commands.open(); }
    else if (k === 'n') { ev.preventDefault(); commands.new(); }
    else if (k === 'w') { ev.preventDefault(); commands.close(); }
    else if (k === 'p') { ev.preventDefault(); commands.saveProject(); }
    else if (k === 'l') { ev.preventDefault(); commands.lock(); }
    else if (k === '=' || k === '+') { ev.preventDefault(); commands.bigger(); }
    else if (k === '-') { ev.preventDefault(); commands.smaller(); }
  });
  // Opens every file given (Open box, launcher, command line), each in its own tab, and
  // says which ones couldn't open. Locked files ask for their password one at a time
  // (review fix #12).
  function openFiles(list) {
    list.filter(function (x) { return x.error; }).forEach(function (x) { toast(x.error, 5000); });
    var good = list.filter(function (x) { return !x.error; });
    (function next(i) {
      if (i >= good.length) return;
      if (!$('pw-modal').hidden) { setTimeout(function () { next(i); }, 500); return; }
      var f = good[i];
      if (!prepareTab(f)) loadDoc(f);
      next(i + 1);
    })(0);
  }
  window.linenPending = function () {
    // Wait while a password box is open; the files are opened once it closes.
    if (!$('pw-modal').hidden) { setTimeout(window.linenPending, 500); return; }
    api('pending').then(function (r) { openFiles(r.files || []); });
  };
  // Called by the window before it closes; it waits for this promise (review fix #2).
  // Saves every tab with unsaved changes, not just the one on screen, then the session.
  window.linenClosing = function () {
    clearTimeout(markDirty.timer); stash();
    return Promise.all(tabs.map(saveTab)).then(function () { return keepSession(); });
  };
  window.addEventListener('beforeunload', function () { window.linenClosing(); });

  // Bring back every tab from last time. Locked notes stay shut (they're left out);
  // saved files reload from disk; unsaved words come from the private session.
  function restoreTabs(ses) {
    var list = ses && ses.tabs ? ses.tabs : ses && (ses.path || ses.text) ? [ses] : [];
    list = list.filter(function (t) { return !t.locked; });
    var wantEntry = ses && ses.tabs ? ses.tabs[ses.current || 0] : null, want = 0;
    return list.reduce(function (p, t) {
      return p.then(function () {
        var load = t.path && !t.dirty ? api('open_path', { path: t.path }).catch(function () { return null; }) : Promise.resolve({ path: t.path, name: t.name, mtime: t.mtime, text: t.text });
        return load.then(function (f) {
          if (!f || (f.locked && typeof f.text !== 'string')) return;
          if (t === wantEntry) want = tabs.length;
          tabs.push({ doc: { path: f.path || '', name: f.name || '', mtime: f.mtime || 0, dirty: !!t.dirty, locked: false }, secret: '', text: f.text || '', scroll: 0 });
        });
      });
    }, Promise.resolve()).then(function () {
      if (!tabs.length) tabs.push(blankTab());
      cur = -1; show(Math.min(want, tabs.length - 1));
      if (doc.dirty) markDirty();
    });
  }

  api('state').then(function (s) {
    state = s; state.settings = s.settings || {}; applySettings();
    var ses = s.session;
    restoreTabs(ses).then(function () {
      api('pending').then(function (r) { openFiles(r.files || []); });
    });
  });
})();
