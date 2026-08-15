/**
 * 4단계 위저드.
 *
 * 화면당 결정 하나. 상태는 단순한 객체 하나로 두고 화면을 통째로 다시 그린다 —
 * 화면이 다섯 개뿐이라 부분 갱신을 최적화할 이유가 없고, 전체 렌더가 상태와
 * 화면이 어긋나는 부류의 버그를 없앤다.
 */

import { invoke } from '@tauri-apps/api/core';
import { t, getLang, setLang, reasonText, type Lang } from './i18n';
import './styles.css';

type DiskEntry = {
  number: number;
  name: string;
  size_bytes: number;
  size_label: string;
  drive_letters: string[];
  ready: boolean;
  blocked_reason: string | null;
  blocked_detail: string | null;
};

type LoaderId = 'MShell' | 'Rr';
type Step = 1 | 2 | 3 | 4 | 5;

type State = {
  step: Step;
  disks: DiskEntry[];
  selectedDisk: number | null;
  loader: LoaderId;
  simulated: boolean;
  loading: boolean;
};

const state: State = {
  step: 1,
  disks: [],
  selectedDisk: null,
  loader: 'MShell',
  simulated: false,
  loading: true,
};

const app = document.querySelector<HTMLDivElement>('#app')!;

/** 선택된 디스크. 목록이 갱신되며 사라졌을 수 있으므로 매번 조회한다. */
function selected(): DiskEntry | undefined {
  return state.disks.find((d) => d.number === state.selectedDisk);
}

function esc(s: string): string {
  return s.replace(
    /[&<>"']/g,
    (c) =>
      ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[
        c
      ]!,
  );
}

/** 줄바꿈을 <br> 로. 제목에만 쓴다. */
function nl(s: string): string {
  return esc(s).replace(/\n/g, '<br>');
}

function segs(active: number): string {
  return `<div class="steps">${[1, 2, 3, 4]
    .map((i) => `<i class="seg${i <= active ? ' on' : ''}"></i>`)
    .join('')}</div>`;
}

function diskItem(d: DiskEntry): string {
  const letters = d.drive_letters.length ? ` · ${d.drive_letters.join(' ')}` : '';
  const sub = d.ready
    ? `${esc(d.size_label)}${esc(letters)}`
    : reasonText(d.blocked_reason ?? '', d.blocked_detail);
  return `
    <button class="item" data-disk="${d.number}" ${d.ready ? '' : 'disabled'}
            aria-selected="${state.selectedDisk === d.number}">
      <span class="body">
        <span class="title">${esc(d.name)}</span>
        <span class="sub${d.ready ? '' : ' warn'}">${esc(sub)}</span>
      </span>
      <span class="radio"></span>
    </button>`;
}

function loaderItem(id: LoaderId, name: string, sub: string, badge: boolean): string {
  return `
    <button class="item" data-loader="${id}" aria-selected="${state.loader === id}">
      <span class="body">
        <span class="title">${badge ? '⭐ ' : ''}${esc(name)}${
          badge ? `<span class="badge">${esc(t('recommended'))}</span>` : ''
        }</span>
        <span class="sub">${esc(sub)}</span>
      </span>
      <span class="radio"></span>
    </button>`;
}

function render() {
  const banner = state.simulated
    ? `<div class="sim-banner">${esc(t('simulated'))}</div>`
    : '';
  const lang = `<div class="lang">
      <button data-lang="ko" aria-pressed="${getLang() === 'ko'}">한국어</button>
      <button data-lang="en" aria-pressed="${getLang() === 'en'}">EN</button>
    </div>`;

  let body = '';
  let foot = '';

  if (state.step === 1) {
    const usable = state.disks.length > 0;
    body = `
      ${segs(1)}
      <main>
        <div class="eyebrow">1 / 4</div>
        <h1>${nl(t('step1_title'))}</h1>
        <p class="lead">${esc(t('step1_lead'))}</p>
        <div class="list">
          ${
            usable
              ? state.disks.map(diskItem).join('')
              : `<div class="empty">${esc(t('step1_empty'))}
                   <div class="hint">${esc(t('step1_empty_hint'))}</div>
                 </div>`
          }
        </div>
      </main>`;
    foot = `<button class="cta" data-go="2" ${
      selected()?.ready ? '' : 'disabled'
    }>${esc(t('next'))}</button>`;
  } else if (state.step === 2) {
    body = `
      ${segs(2)}
      <main>
        <div class="eyebrow">2 / 4</div>
        <h1>${nl(t('step2_title'))}</h1>
        <p class="lead">${esc(t('step2_lead'))}</p>
        <div class="list">
          ${loaderItem('MShell', 'm-shell', t('mshell_sub'), true)}
          ${loaderItem('Rr', 'RR', t('rr_sub'), false)}
        </div>
      </main>`;
    foot = `<button class="ghost" data-go="1">${esc(t('back'))}</button>
            <button class="cta" data-go="3">${esc(t('next'))}</button>`;
  } else if (state.step === 3) {
    const d = selected();
    body = `
      ${segs(3)}
      <main>
        <div class="eyebrow">3 / 4</div>
        <h1 class="danger">${nl(t('step3_title'))}</h1>
        <p class="lead">${esc(t('step3_lead'))}</p>
        <div class="target">
          <div class="name">${esc(d?.name ?? '')}</div>
          <div class="meta">${esc(d?.size_label ?? '')}${
            d?.drive_letters.length ? ` · ${esc(d.drive_letters.join(' '))}` : ''
          }</div>
        </div>
        <div class="note"><span>ℹ</span><span>${esc(t('step3_note'))}</span></div>
      </main>`;
    foot = `<button class="ghost" data-go="2">${esc(t('back'))}</button>
            <button class="cta danger" data-go="4">${esc(
              t('erase_and_write'),
            )}</button>`;
  } else if (state.step === 4) {
    body = `
      ${segs(4)}
      <main>
        <div class="eyebrow">4 / 4</div>
        <h1>${nl(t('step4_title'))}</h1>
        <p class="lead">${esc(t('step4_lead'))}</p>
      </main>`;
    foot = `<button class="ghost" data-go="3">${esc(t('cancel'))}</button>`;
  } else {
    body = `
      ${segs(4)}
      <main>
        <h1>${nl(t('done_title'))}</h1>
        <p class="lead">${esc(t('done_lead'))}</p>
      </main>`;
    foot = `<button class="cta" data-go="1">${esc(t('done'))}</button>`;
  }

  // 배너가 맨 위, 그 아래 언어 전환. 순서가 겹침을 막는다.
  app.innerHTML = `${banner}${lang}${body}<footer>${foot}</footer>`;
}

app.addEventListener('click', (e) => {
  const el = (e.target as HTMLElement).closest<HTMLElement>('[data-disk],[data-loader],[data-go],[data-lang]');
  if (!el) return;

  if (el.dataset.disk) {
    state.selectedDisk = Number(el.dataset.disk);
  } else if (el.dataset.loader) {
    state.loader = el.dataset.loader as LoaderId;
  } else if (el.dataset.lang) {
    setLang(el.dataset.lang as Lang);
  } else if (el.dataset.go) {
    state.step = Number(el.dataset.go) as Step;
  }
  render();
});

/**
 * Tauri 밖(그냥 브라우저)에서 열렸는가.
 *
 * UI 를 다듬을 때 앱을 매번 다시 빌드하지 않고 `npm run dev` 로 브라우저에서
 * 확인하기 위한 것이다. Tauri 안에서는 항상 false 이므로 배포물에는 영향이 없다.
 */
function isBrowserPreview(): boolean {
  return !('__TAURI_INTERNALS__' in window);
}

/** 브라우저 미리보기용 표본. Rust 쪽 FakeEnumerator 와 같은 장치들. */
const previewDisks: DiskEntry[] = [
  {
    number: 2,
    name: 'SanDisk Ultra USB 3.0',
    size_bytes: 30_752_000_000,
    size_label: '30.8 GB',
    drive_letters: ['E:'],
    ready: true,
    blocked_reason: null,
    blocked_detail: null,
  },
  {
    number: 3,
    name: 'Samsung Flash Drive FIT',
    size_bytes: 64_055_500_800,
    size_label: '64.1 GB',
    drive_letters: [],
    ready: true,
    blocked_reason: null,
    blocked_detail: null,
  },
  {
    number: 4,
    name: 'Generic Flash Disk',
    size_bytes: 4_004_511_744,
    size_label: '4.00 GB',
    drive_letters: ['F:'],
    ready: false,
    blocked_reason: 'too_small_for_any_image',
    blocked_detail: '8.00 GB',
  },
];

async function boot() {
  try {
    if (isBrowserPreview()) {
      state.simulated = true;
      state.disks = previewDisks;
    } else {
      state.simulated = await invoke<boolean>('is_simulated');
      state.disks = await invoke<DiskEntry[]>('list_disks');
    }
    // 선택 가능한 것이 하나뿐이면 미리 골라둔다. 흔한 경우라 클릭을 아낀다.
    const ready = state.disks.filter((d) => d.ready);
    if (ready.length === 1) state.selectedDisk = ready[0].number;
  } catch (err) {
    console.error('열거 실패', err);
  } finally {
    state.loading = false;
    render();
  }
}

boot();
