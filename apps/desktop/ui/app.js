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

  // How the connection looked once hosting started.
  const connection = status.hosting ? status.connection : null;
  const said = (ok, good, bad) => (ok === null || ok === undefined ? null : ok ? good : bad);
  const parts = connection
    ? [
        said(connection.tunnel, 'players can reach this computer', 'the tunnel isn\'t answering'),
        said(connection.website, 'the website answers', 'the website can\'t be reached'),
        connection.uploadMbps != null ? `upload about ${connection.uploadMbps} Mbit/s` : null,
      ].filter(Boolean)
    : [];
  $('connection').hidden = parts.length === 0;
  $('connection').textContent = parts.length ? `Connection: ${parts.join(' · ')}.` : '';
  $('slow').hidden = !connection?.slow;
  $('slow').textContent = connection?.slow
    ? 'Your upload is slow for hosting: with more than a few players, maps and pictures will ' +
      'come slowly. A wired connection or a quieter network helps.'
    : '';

  $('waiting-card').hidden = status.requests.length === 0;
  $('waiting').replaceChildren(
    ...status.requests.map((request) => {
      const row = document.createElement('li');
      row.className = 'request';
      const who = document.createElement('span');
      const name = document.createElement('strong');
      name.textContent = request.name;
      const what = document.createElement('span');
      what.className = 'muted';
      what.textContent = ` ${request.classes || 'No class yet'} · ${request.table}${request.account ? '' : ' · guest'}`;
      who.append(name, what);
      const buttons = document.createElement('span');
      buttons.className = 'row';
      for (const [label, accept] of [['Let in', true], ['Turn away', false]]) {
        const button = document.createElement('button');
        button.type = 'button';
        button.className = accept ? 'button button--main' : 'button';
        button.textContent = label;
        button.addEventListener('click', () =>
          run('answer_request', { tableId: request.tableId, id: request.id, accept }),
        );
        buttons.append(button);
      }
      row.append(who, buttons);
      return row;
    }),
  );
  $('offer').hidden = !status.linkOffer;
  $('offer-website').textContent = status.linkOffer ?? '';
  $('paired').hidden = !status.website || status.pairing;
  $('website').textContent = status.website ?? '';
  $('unpaired').hidden = !!status.website || status.pairing;
  $('by-code').hidden = !!status.website || status.pairing;
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
      const save = document.createElement('button');
      save.type = 'button';
      save.className = 'link';
      save.textContent = 'Back up';
      save.addEventListener('click', () => backUp(table));
      row.append(name, seats, save);
      return row;
    }),
  );
  $('no-tables').hidden = status.tables.length > 0;

  const skipped = status.librarySkipped;
  $('library-skipped').hidden = skipped.length === 0;
  $('library-skipped').textContent =
    skipped.length === 1
      ? `One file in the picture library couldn't be added: ${skipped[0]}`
      : `${skipped.length} files in the picture library couldn't be added: ${skipped.join('; ')}`;

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

function notice(text) {
  $('notice').hidden = !text;
  $('notice').textContent = text ?? '';
}

async function backUp(table) {
  try {
    const where = await invoke('backup_table', { id: table.id, name: table.name });
    if (where) notice(`${table.name} is saved in ${where}`);
  } catch (error) {
    notice(`Couldn't back up ${table.name}: ${error}`);
  }
}

$('restore').addEventListener('click', async () => {
  try {
    const name = await invoke('restore_table');
    if (name) notice(`${name} is back on this computer.`);
    await refresh();
  } catch (error) {
    notice(`Couldn't restore it: ${error}`);
  }
});
$('offer-yes').addEventListener('click', () => run('answer_link', { accept: true }));
$('offer-no').addEventListener('click', () => run('answer_link', { accept: false }));
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
$('library').addEventListener('click', () => run('open_library_folder'));
$('backups').addEventListener('click', () => run('open_backups_folder'));

$('pair-website').value = localStorage.getItem('website') ?? '';
refresh();
setInterval(refresh, 2000);
