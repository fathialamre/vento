// Animated product scenes: a drawn file manager window, the Vento wheel and the island.
// Each [data-scene] element plays its phases in a loop while it is on screen.
// Every scene carries the macOS, Windows and Linux chrome at once; the
// page's data-os picks which one shows (.only-mac, .only-win, .only-lin).

const FOLDER = '<svg viewBox="0 0 64 50"><path d="M3 7a4 4 0 0 1 4-4h15l5 5h30a4 4 0 0 1 4 4v31a4 4 0 0 1-4 4H7a4 4 0 0 1-4-4z" fill="#7DC0FA"/><path d="M3 15a3 3 0 0 1 3-3h52a3 3 0 0 1 3 3v28a4 4 0 0 1-4 4H7a4 4 0 0 1-4-4z" fill="#3E9BF2"/></svg>';
const doc = (tag, color) => `<svg viewBox="0 0 44 54"><path d="M5 1.5h24l13 13V50a2.5 2.5 0 0 1-2.5 2.5H5A2.5 2.5 0 0 1 2.5 50V4A2.5 2.5 0 0 1 5 1.5z" fill="#fff" stroke="#D3D8E1"/><path d="M29 1.5V12a2.5 2.5 0 0 0 2.5 2.5H42" fill="#EEF1F6" stroke="#D3D8E1"/><text x="22" y="40" font-size="8" font-weight="700" text-anchor="middle" fill="${color}" font-family="Inter,sans-serif">${tag}</text></svg>`;
// Explorer's manila folder and GNOME's folder, shown in place of the Finder one.
const FOLDER_WIN = '<svg viewBox="0 0 64 50"><path d="M3 6a3 3 0 0 1 3-3h17l5 5h30a3 3 0 0 1 3 3v33a3 3 0 0 1-3 3H6a3 3 0 0 1-3-3z" fill="#E8A92E"/><path d="M3 14a3 3 0 0 1 3-3h52a3 3 0 0 1 3 3v30a3 3 0 0 1-3 3H6a3 3 0 0 1-3-3z" fill="#FFC83D"/></svg>';
const FOLDER_LIN = '<svg viewBox="0 0 64 50"><path d="M3 7a4 4 0 0 1 4-4h14l5 5h31a4 4 0 0 1 4 4v31a4 4 0 0 1-4 4H7a4 4 0 0 1-4-4z" fill="#438DE6"/><path d="M3 16a3 3 0 0 1 3-3h52a3 3 0 0 1 3 3v27a4 4 0 0 1-4 4H7a4 4 0 0 1-4-4z" fill="#62A0EA"/><path d="M3 16a3 3 0 0 1 3-3h52a3 3 0 0 1 3 3v2H3z" fill="#99C1F1"/></svg>';
const folder = `<span class="only-mac">${FOLDER}</span><span class="only-win">${FOLDER_WIN}</span><span class="only-lin">${FOLDER_LIN}</span>`;
const ICON = { folder, txt: doc('TXT', '#8A93A5'), zip: doc('ZIP', '#8A93A5'), pdf: doc('PDF', '#E5484D') };
const CURSOR = '<svg width="22" height="30" viewBox="0 0 22 30"><path d="M2 2v21l5.6-5.2 3.7 8.6 3.8-1.6-3.7-8.5H19z" fill="#111" stroke="#fff" stroke-width="1.6" stroke-linejoin="round"/></svg>';
const HEX = '<svg viewBox="0 0 100 87"><polygon points="27,5 73,5 95,43.5 73,82 27,82 5,43.5"/></svg>';

// Neighbour offsets of a flat-top hexagon, in stage pixels.
const DIR = { T: [0, -77], UR: [66.8, -38.6], LR: [66.8, 38.6], B: [0, 77], LL: [-66.8, 38.6], UL: [-66.8, -38.6] };
const BASE = [['Pocket', 'ti-stack-2', 'T'], ['Copy', 'ti-copy', 'UR'], ['Move', 'ti-folder-symlink', 'LR'], ['Copy path', 'ti-link', 'B'], ['Terminal', 'ti-terminal-2', 'LL']];
const FILES = [['Clients', 'folder'], ['Notes.txt', 'txt'], ['Photos', 'folder'], ['Private.zip', 'zip'], ['Projects', 'folder'], ['Quarterly Report.pdf', 'pdf']];
const CENTER = [480, 390];
const IDLE = [700, 540];

const SCENES = {
  copy: [{
    src: 5, path: ['Copy'],
    toast: 'Copied 1 item. Paste it anywhere.',
  }],
  pocket: [{
    src: 1, path: ['Pocket'], island: true,
  }],
  move: [{
    src: 5, path: ['Move', 'Projects'],
    ring: { at: 'Move', items: [['Photos', 'UR'], ['Projects', 'LR'], ['Clients', 'B']] },
    hud: ['Move to Projects', '<span class="only-mac only-lin">~/Desktop/vento-demo/Projects</span><span class="only-win">C:\\Users\\you\\Desktop\\vento-demo\\Projects</span>'],
    toast: 'Moved 1 item to Projects', undo: true,
    drop: files => { files[5] = null; },
  }],
  archive: [{
    src: 2, path: ['Compress'], extra: ['Compress', 'ti-file-zip', 'UL'],
    toast: 'Compressed 1 item to Photos.zip', undo: true,
    drop: files => { files[2] = ['Photos.zip', 'zip']; },
  }, {
    src: 2, path: ['Extract'], extra: ['Extract', 'ti-package-export', 'UL'],
    toast: 'Extracted Photos.zip', undo: true,
    drop: files => { files[2] = ['Photos', 'folder']; },
  }],
};

const sleep = ms => new Promise(r => setTimeout(r, ms));
const itemX = i => 168 + i * 108;

function build(el, phases) {
  const pocket = phases.some(p => p.island);
  el.innerHTML = `<div class="stage">
    <div class="mbar only-mac"><span><i class="ti ti-brand-apple"></i><b>Finder</b>File&nbsp; Edit&nbsp; View&nbsp; Go</span><span><i class="ti ti-hexagons"></i><i class="ti ti-wifi"></i><i class="ti ti-battery-3"></i>Sun 9:41</span></div>
    <div class="gbar only-lin"><span class="ws"><i></i><i></i><i></i></span><span class="clock">Sun 9:41</span><span class="tray"><i class="ti ti-hexagons"></i><i class="ti ti-wifi"></i><i class="ti ti-volume"></i><i class="ti ti-battery-3"></i></span></div>
    <div class="taskbar only-win"><span class="apps"><i class="ti ti-layout-grid start"></i><i class="ti ti-search"></i><i class="ti ti-folder on"></i><i class="ti ti-world"></i><i class="ti ti-mail"></i></span><span class="tray"><i class="ti ti-chevron-up"></i><i class="ti ti-hexagons"></i><i class="ti ti-wifi"></i><i class="ti ti-volume"></i><span class="clock">9:41<br>11/4/2026</span></span></div>
    ${pocket ? `<div class="island"><div class="mini"><i class="ti ti-stack-2"></i><span>1</span></div><div class="full"><div class="chip"></div><span class="hint">Drag it out when<br>you get there</span><div class="badge"><i class="ti ti-hand-grab"></i><b>1</b></div></div></div>` : ''}
    <div class="win">
      <div class="tb only-mac"><i class="d r"></i><i class="d y"></i><i class="d g"></i><span class="nav2"><i class="ti ti-chevron-left"></i><i class="ti ti-chevron-right"></i></span>vento-demo</div>
      <div class="tb wtb only-win"><span class="tab">${FOLDER_WIN}vento-demo<i class="ti ti-x"></i></span><i class="ti ti-plus add"></i><span class="caps"><i class="ti ti-minus"></i><i class="ti ti-square"></i><i class="ti ti-x"></i></span></div>
      <div class="addr only-win"><i class="ti ti-arrow-left"></i><i class="ti ti-arrow-right"></i><i class="ti ti-arrow-up"></i><span class="path">${FOLDER_WIN}<i class="ti ti-chevron-right"></i>Desktop<i class="ti ti-chevron-right"></i>vento-demo</span><span class="find"><i class="ti ti-search"></i>Search vento-demo</span></div>
      <div class="tb ltb only-lin"><span class="btn"><i class="ti ti-chevron-left"></i></span><span class="btn"><i class="ti ti-chevron-right"></i></span><span class="path"><i class="ti ti-home"></i>Desktop<b>/</b>vento-demo</span><span class="btn"><i class="ti ti-search"></i></span><span class="btn"><i class="ti ti-menu-2"></i></span><span class="close"><i class="ti ti-x"></i></span></div>
    </div>
    <div class="items"></div>
    <div class="wheel"></div>
    <div class="ghost"></div>
    <div class="cur">${CURSOR}</div>
    <div class="kc"><span class="only-mac">control ⌃</span><span class="only-win only-lin">Ctrl</span></div>
    <div class="hud"></div>
    <div class="toast"></div>
  </div>`;
  return el.firstElementChild;
}

function play(el) {
  const phases = SCENES[el.dataset.scene];
  const st = build(el, phases);
  const $ = s => st.querySelector(s);
  new ResizeObserver(() => st.style.setProperty('--s', el.clientWidth / 960)).observe(el);

  let files;
  const renderFiles = () => {
    $('.items').innerHTML = files.map((f, i) => f
      ? `<div class="it" data-i="${i}" style="left:${itemX(i)}px">${ICON[f[1]]}<span class="lb">${f[0]}</span></div>`
      : '').join('');
  };
  const move = ([x, y], t) => {
    st.style.setProperty('--t', t + 'ms');
    st.style.setProperty('--cx', x);
    st.style.setProperty('--cy', y);
  };
  const set = s => { st.className = 'stage ' + s; };
  const add = s => st.classList.add(s);

  // Positions of every hex in this phase, keyed by name.
  let pos;
  const renderWheel = ph => {
    const hexes = BASE.concat(ph.extra ? [ph.extra] : []);
    pos = {};
    let html = `<div class="hex center" style="--x:0;--y:0;--i:0">${HEX}<span class="cl"></span></div>`;
    hexes.forEach(([n, ic, d], i) => {
      const [x, y] = DIR[d];
      pos[n] = [x, y];
      const anchor = ph.ring && ph.ring.at === n ? ' anchor' : '';
      html += `<div class="hex${anchor}" data-n="${n}" style="--x:${x};--y:${y};--i:${i + 1}">${HEX}<i class="ti ${ic}"></i>${n}</div>`;
    });
    if (ph.ring) {
      const [ax, ay] = pos[ph.ring.at];
      ph.ring.items.forEach(([n, d], i) => {
        const x = ax + DIR[d][0], y = ay + DIR[d][1];
        pos[n] = [x, y];
        html += `<div class="hex ring" data-n="${n}" style="--x:${x};--y:${y};--i:${i}">${HEX}${folder}${n}</div>`;
      });
    }
    $('.wheel').innerHTML = html;
  };
  const hover = n => {
    st.querySelectorAll('.hex.hv').forEach(h => h.classList.remove('hv'));
    if (!n) return;
    st.querySelector(`.hex[data-n="${n}"]`).classList.add('hv');
    if (!st.classList.contains('ring-on')) $('.cl').textContent = n;
  };
  const at = n => [CENTER[0] + pos[n][0], CENTER[1] + pos[n][1]];

  const visible = () => new Promise(resolve => {
    const io = new IntersectionObserver(([e]) => { if (e.isIntersecting) { io.disconnect(); resolve(); } });
    io.observe(el);
  });

  async function phase(ph) {
    const f = files[ph.src];
    renderWheel(ph);
    $('.ghost').innerHTML = `${ICON[f[1]]}<span class="lb">${f[0]}</span>`;
    $('.cl').textContent = '';
    if (ph.island) $('.chip').innerHTML = ICON[f[1]] + f[0];
    $('.toast').innerHTML = ph.toast ? `${ph.toast}${ph.undo ? '<b>Undo</b>' : ''}` : '';
    if (ph.hud) $('.hud').innerHTML = `${folder}<span><b>${ph.hud[0]}</b><br>${ph.hud[1]}</span>`;
    set('');
    const src = st.querySelector(`.it[data-i="${ph.src}"]`);
    src.classList.add('src');

    move(IDLE, 0); await sleep(450);
    move([itemX(ph.src) + 48, 174], 800); await sleep(900);
    set('sel'); await sleep(300);
    set('sel drag'); move(CENTER, 900); await sleep(1100);
    set('sel drag ctrl wheel-on'); await sleep(650);

    for (const n of ph.path) {
      move(at(n), 420); await sleep(450);
      hover(n);
      if (ph.ring && n === ph.ring.at) {
        await sleep(700);
        add('ring-on');
        $('.cl').innerHTML = ICON[f[1]];
        await sleep(650);
      }
      else { if (ph.hud) add('hud-on'); await sleep(750); }
    }

    hover(null);
    set(ph.island ? 'done' : 'done toast-on');
    if (ph.drop) { ph.drop(files); renderFiles(); }
    move([st.style.getPropertyValue('--cx') * 1 + 50, st.style.getPropertyValue('--cy') * 1 + 70], 700);
    await sleep(2300);
    set(ph.island ? 'parked' : 'done');
    await sleep(700);
  }

  if (matchMedia('(prefers-reduced-motion: reduce)').matches) {
    const ph = phases[0];
    files = FILES.slice(); renderFiles(); renderWheel(ph);
    const f = files[ph.src];
    $('.ghost').innerHTML = `${ICON[f[1]]}<span class="lb">${f[0]}</span>`;
    st.querySelector(`.it[data-i="${ph.src}"]`).classList.add('src');
    move(at(ph.path[0]), 0); hover(ph.path[0]);
    set('sel drag ctrl wheel-on');
    return;
  }

  (async () => {
    for (;;) {
      await visible();
      files = FILES.slice(); renderFiles();
      for (const ph of phases) await phase(ph);
      await sleep(500);
    }
  })();
}

// The virtual island on a screen with no notch: it rests as a glass pill,
// plays a shimmer, says what a job is doing, opens when the pointer pushes
// against the top edge, and can rest as a circle or sit elsewhere along the
// edge. The pills are renders of the real island (assets/vi-*.png).
function playVirtual(el) {
  el.innerHTML = `<div class="stage vstage">
    <div class="vwin"><div class="vtabs"><span class="on"><i class="ti ti-world"></i>Trip notes</span><span><i class="ti ti-world"></i>Flights</span><span><i class="ti ti-world"></i>Hotels</span><span class="caps"><i class="ti ti-minus"></i><i class="ti ti-square"></i><i class="ti ti-x"></i></span></div>
      <div class="vbody"><b></b><i></i><i></i><i class="s"></i><i></i><i class="s"></i></div></div>
    <div class="taskbar"><span class="apps"><i class="ti ti-layout-grid start"></i><i class="ti ti-search"></i><i class="ti ti-folder"></i><i class="ti ti-world on"></i><i class="ti ti-mail"></i></span><span class="tray"><i class="ti ti-chevron-up"></i><i class="ti ti-hexagons"></i><i class="ti ti-wifi"></i><i class="ti ti-volume"></i><span class="clock">9:41<br>11/4/2026</span></span></div>
    <div class="vis" style="--vx:480">
      <img class="p rest" src="assets/vi-rest.png" alt="">
      <img class="p circle" src="assets/vi-circle.png" alt="">
      <img class="p work" src="assets/vi-work.png" alt="">
      <img class="p done" src="assets/vi-done.png" alt="">
      <span class="shine"></span><span class="push"></span>
      <img class="open" src="assets/island-clipboard.png" alt="">
    </div>
    <div class="cur">${CURSOR}</div>
    <div class="cap"></div>
  </div>`;
  const st = el.firstElementChild, vi = st.querySelector('.vis'), cap = st.querySelector('.cap');
  new ResizeObserver(() => st.style.setProperty('--s', el.clientWidth / 960)).observe(el);
  const move = ([x, y], t) => {
    st.style.setProperty('--t', t + 'ms');
    st.style.setProperty('--cx', x);
    st.style.setProperty('--cy', y);
  };
  const show = (state, text) => { vi.dataset.v = state; cap.textContent = text || ''; cap.classList.toggle('on', !!text); };
  const place = (x, t) => { vi.style.setProperty('--t', t + 'ms'); vi.style.setProperty('--vx', x); };

  if (matchMedia('(prefers-reduced-motion: reduce)').matches) { show('done', 'Says what each job is doing'); move([700, 420], 0); return; }

  const visible = () => new Promise(resolve => {
    const io = new IntersectionObserver(([e]) => { if (e.isIntersecting) { io.disconnect(); resolve(); } });
    io.observe(el);
  });
  (async () => {
    for (;;) {
      await visible();
      place(480, 0); move([700, 420], 0);
      show('rest', 'Rests as glass, out of your way'); await sleep(1600);
      show('rest shimmer', 'Now and then, a shimmer'); await sleep(2200);
      show('work', 'Tells you what a job is doing'); await sleep(2400);
      show('done', 'Then the result, with one button'); await sleep(2600);
      show('rest', 'Push against the top edge'); move([500, 120], 900); await sleep(1000);
      move([500, 2], 500); await sleep(500);
      show('rest pushing', 'Push against the top edge'); await sleep(900);
      show('opened', 'and it opens'); move([640, 330], 900); await sleep(2900);
      show('rest', 'Island or Circle'); await sleep(900);
      show('circle', 'Island or Circle'); await sleep(1800);
      show('rest', 'Left, Centre, Right, or drag it anywhere'); place(170, 900); await sleep(1700);
      place(720, 1100); await sleep(1900);
      place(480, 900); await sleep(1200);
    }
  })();
}

document.querySelectorAll('[data-scene]').forEach(el => el.dataset.scene === 'virtual' ? playVirtual(el) : play(el));
