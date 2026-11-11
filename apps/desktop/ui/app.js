// The window's page. It asks the app how things are every two seconds and draws that; every
// button is one command to the app.
const { invoke } = window.__TAURI__.core;
const $ = (id) => document.getElementById(id);

let latest = null;
let busy = false;

function draw(status) {
  latest = status;
  const working = status.starting || busy;
  $('dot').className = working ? 'dot is-busy' : status.hosting ? 'dot is-on' : 'dot';
  if (status.starting) {
    $('state-title').textContent = 'Starting...';
    $('state-note').textContent = 'The first start gets the tunnel program, which takes a moment.';
  } else if (status.hosting && status.address) {
    $('state-title').textContent = 'Hosting is on';
    const screens = status.screens === 1 ? '1 screen has' : `${status.screens} screens have`;
    $('state-note').textContent = `Players can join from the website. ${screens} a table open.`;
  } else if (status.hosting) {
    $('state-title').textContent = 'Hosting is on';
    $('state-note').textContent = 'Opening the tunnel so players can reach this computer...';
  } else {
    $('state-title').textContent = 'Hosting is off';
    $('state-note').textContent = status.website
      ? 'Your tables are safe here. Players can join once you start hosting.'
      : 'Connect to your account below, then start hosting.';
  }
  const toggle = $('toggle');
  toggle.textContent = status.hosting ? 'Stop hosting' : 'Start hosting';
  toggle.classList.toggle('is-on', status.hosting);
  toggle.disabled = working || (!status.hosting && !status.website);

  $('problem').hidden = !status.problem;
  $('problem').textContent = status.problem ?? '';

  $('paired').hidden = !status.website || status.pairing;
  $('website').textContent = status.website ?? '';
  $('pair-form').hidden = !!status.website || status.pairing;
  $('pairing').hidden = !status.pairing;
  $('code').textContent = status.pairingCode ?? '....-....';

  const list = $('tables');
  list.replaceChildren(
    ...status.tables.map((table) => {
      const row = document.createElement('li');
      const name = document.createElement('strong');
      name.textContent = table.name;
      const seats = document.createElement('span');
      seats.className = 'muted';
      seats.textContent = `${table.seats} ${table.seats === 1 ? 'player' : 'players'}`;
      row.append(name, seats);
      return row;
    }),
  );
  $('no-tables').hidden = status.tables.length > 0;

  $('start-on-open').checked = status.startOnOpen;
  $('tray-note').textContent = status.hasTray
    ? 'Closing this window keeps the app in the tray. Quit from the tray menu.'
    : 'This system shows no tray icon, so closing this window only minimizes it.';
  $('version').textContent = `Version ${status.version} · ${status.dataFolder}`;
}

async function refresh() {
  try {
    draw(await invoke('status'));
  } catch (error) {
    $('problem').hidden = false;
    $('problem').textContent = String(error);
  }
}

// Runs a command, showing the busy dot meanwhile; a refusal shows as the problem line.
async function run(command, args) {
  busy = true;
  if (latest) draw(latest);
  try {
    await invoke(command, args);
  } catch {
    // The app keeps the reason; the next status shows it.
  }
  busy = false;
  await refresh();
}

$('toggle').addEventListener('click', () => run(latest?.hosting ? 'stop_hosting' : 'start_hosting'));
$('pair-form').addEventListener('submit', (event) => {
  event.preventDefault();
  localStorage.setItem('website', $('pair-website').value);
  run('pair', { websiteUrl: $('pair-website').value, name: $('pair-name').value });
});
$('unpair').addEventListener('click', () => run('unpair'));
$('start-on-open').addEventListener('change', (event) =>
  run('set_start_on_open', { on: event.target.checked }),
);
$('folder').addEventListener('click', () => run('open_data_folder'));

$('pair-website').value = localStorage.getItem('website') ?? '';
refresh();
setInterval(refresh, 2000);
