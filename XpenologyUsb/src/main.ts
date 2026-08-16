/**
 * 4단계 위저드.
 *
 * 화면당 결정 하나. 상태는 단순한 객체 하나로 두고 화면을 통째로 다시 그린다 —
 * 화면이 다섯 개뿐이라 부분 갱신을 최적화할 이유가 없고, 전체 렌더가 상태와
 * 화면이 어긋나는 부류의 버그를 없앤다.
 */

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { t, getLang, setLang, type Lang } from './i18n';
import {
  ACTION_SELECTOR,
  ACTIONS,
  diskItem,
  ejectBlock,
  esc,
  fmtBytes,
  fmtEta,
  nl,
  normalizeFailure,
  segs,
  type DiskEntry,
  type Failure,
  type ProgressEvent,
  type Stage,
} from './ui';
import './styles.css';

type LoaderId = 'MShell' | 'Rr';
type Step = 1 | 2 | 3 | 4 | 5 | 6;

type RunSummary = {
  loader: string;
  tag: string;
  asset_name: string;
  bytes_written: number;
  verified: boolean;
};

type State = {
  step: Step;
  disks: DiskEntry[];
  selectedDisk: number | null;
  loader: LoaderId;
  verify: boolean;
  simulated: boolean;
  loading: boolean;
  progress: ProgressEvent | null;
  failure: Failure | null;
  summary: RunSummary | null;
  /** 안전 제거 상태. null 이면 아직 누르지 않은 것. */
  eject: 'busy' | 'ok' | 'fail' | null;
  /** 목록을 다시 읽는 중인가. */
  scanning: boolean;
};

const state: State = {
  step: 1,
  disks: [],
  selectedDisk: null,
  loader: 'MShell',
  verify: false,
  simulated: false,
  loading: true,
  progress: null,
  failure: null,
  summary: null,
  eject: null,
  scanning: false,
};

/** 실행할 단계 순서. 검증은 선택이라 켰을 때만 들어간다. */
function plannedStages(): Stage[] {
  const s: Stage[] = [
    'Resolving',
    'Downloading',
    'Extracting',
    'Preparing',
    'Writing',
  ];
  if (state.verify) s.push('Verifying');
  s.push('Finishing');
  return s;
}

const app = document.querySelector<HTMLDivElement>('#app')!;

/** 선택된 디스크. 목록이 갱신되며 사라졌을 수 있으므로 매번 조회한다. */
function selected(): DiskEntry | undefined {
  return state.disks.find((d) => d.number === state.selectedDisk);
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
        <div class="row-head">
          <h1>${nl(t('step1_title'))}</h1>
          <button class="refresh" data-refresh="1" ${
            state.scanning ? 'disabled' : ''
          } title="${esc(t('refresh'))}">${state.scanning ? '⋯' : '↻'}</button>
        </div>
        <p class="lead">${esc(t('step1_lead'))}</p>
        <div class="list">
          ${
            usable
              ? state.disks
                  .map((d) => diskItem(d, state.selectedDisk))
                  .join('')
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
    const p = state.progress;
    const stages = plannedStages();

    // 각 단계를 완료 / 진행 중 / 대기로 그린다.
    // 사용자가 "지금 뭐 하는 중인지" 한눈에 보게 하는 것이 목적이다.
    const list = stages
      .map((s) => {
        const done = p?.completed.includes(s) ?? false;
        const active = p?.stage === s;
        const mark = done ? '✓' : active ? '›' : '·';
        // 진행 중인 단계에만 속도나 부가 정보를 붙인다.
        let extra = '';
        if (active && p) {
          // 오래 걸리는 단계에는 속도를 붙인다. 몇 분씩 걸리는 쓰기 단계에
          // 아무 숫자도 없으면 멈춘 것처럼 보인다.
          const timed =
            p.stage === 'Downloading' ||
            p.stage === 'Writing' ||
            p.stage === 'Verifying';
          if (timed && p.bytes_per_sec) {
            extra = t('speed', fmtBytes(p.bytes_per_sec));
          } else if (p.detail) {
            extra = p.detail;
          }
        }
        return `<div class="stage${done ? ' done' : ''}${active ? ' active' : ''}">
            <span class="mark">${mark}</span>
            <span>${esc(t(`stage_${s}`))}</span>
            ${extra ? `<span class="extra">${esc(extra)}</span>` : ''}
          </div>`;
      })
      .join('');

    // 총량을 모르면 불확정 막대로 바꾼다. 0% 에 멈춘 막대는
    // 멈춘 것처럼 보여서 사용자가 창을 닫는다.
    const indeterminate = p == null || p.percent == null;
    const width = p?.percent ?? 0;
    const bar = `
      <div class="bar${indeterminate ? ' indeterminate' : ''}">
        <i style="width:${indeterminate ? '' : `${width}%`}"></i>
      </div>
      <div class="bar-meta">
        <span class="pct">${indeterminate ? '' : `${width}%`}</span>
        <span>${
          p && p.total_bytes
            ? esc(t('of_total', fmtBytes(p.done_bytes), fmtBytes(p.total_bytes)))
            : ''
        }</span>
        <span>${p?.eta_secs != null ? esc(fmtEta(p.eta_secs)) : ''}</span>
      </div>`;

    body = `
      ${segs(4)}
      <main>
        <div class="eyebrow">4 / 4</div>
        <h1>${nl(t('step4_title'))}</h1>
        <p class="lead">${esc(t('step4_lead'))}</p>
        <div class="stages">${list}</div>
        ${bar}
      </main>`;
    // 취소는 화면만 되돌리는 것이 아니라 백엔드 작업을 실제로 멈춰야 한다.
    // 쓰기가 계속 도는데 화면만 3단계로 가면 사용자가 USB 를 뽑는다.
    foot = `<button class="ghost" data-cancel="1">${esc(t('cancel'))}</button>`;
  } else if (state.step === 6) {
    // 실패 화면. 원인마다 다음에 뭘 해야 하는지 함께 알려준다.
    const f = state.failure;
    const code = f?.code ?? 'generic';
    const what = t(`err_${code}`) === `err_${code}` ? t('error_title') : t(`err_${code}`);
    const why =
      t(`err_${code}_why`) === `err_${code}_why`
        ? t('err_generic_why')
        : t(`err_${code}_why`);
    body = `
      ${segs(4)}
      <main>
        <h1 class="danger">${nl(t('error_title'))}</h1>
        <div class="error-box">
          <div class="what">${esc(what)}</div>
          <div class="why">${esc(why)}</div>
          ${f?.detail ? `<div class="why">${esc(f.detail)}</div>` : ''}
        </div>
      </main>`;
    foot = `<button class="ghost" data-go="1">${esc(t('back'))}</button>
            <button class="cta" data-go="3">${esc(t('retry'))}</button>`;
  } else {
    const sm = state.summary;
    // 무엇이 얼마나 쓰였는지 보여준다. "성공했습니다" 한 줄만 있으면,
    // 탐색기에서 USB 내용이 보이지 않는 것을 보고 실패했다고 판단하게 된다.
    const written = sm
      ? `<div class="written">${esc(
          t('done_written', sm.loader, sm.tag, fmtBytes(sm.bytes_written)),
        )}</div>`
      : '';
    const verified =
      sm?.verified
        ? `<div class="written ok">✓ ${esc(t('done_verified'))}</div>`
        : '';
    body = `
      ${segs(4)}
      <main>
        <div class="center">
          <div class="tick">✓</div>
          <h1>${nl(t('done_title'))}</h1>
          <p class="lead">${esc(t('done_lead'))}</p>
          ${written}
          ${verified}
          ${ejectBlock(state.eject)}
        </div>
        <div class="note explain">
          <span>ℹ</span>
          <span><b>${esc(t('done_explorer_title'))}</b><br>${esc(
            t('done_explorer_body'),
          )}<br><span class="dim">${esc(t('done_replug'))}</span></span>
        </div>
      </main>`;
    foot = `<button class="cta" data-go="1">${esc(t('done'))}</button>`;
  }

  // 배너가 맨 위, 그 아래 언어 전환. 순서가 겹침을 막는다.
  app.innerHTML = `${banner}${lang}${body}<footer>${foot}</footer>`;
  warnUnhandledActions();
}

/**
 * 화면에 있는데 클릭 처리기가 못 받는 버튼이 있는지 검사한다.
 *
 * 이 검사가 없었다면 안전 제거 버튼이 동작하지 않는 것을 사용자가 눌러보고
 * 알려줄 때까지 몰랐을 것이다. 실제로 그랬다.
 *
 * 화면당 버튼이 몇 개뿐이라 항상 돌려도 비용이 없다. 콘솔에만 남으므로
 * 사용자에게는 보이지 않지만, 개발 중에는 즉시 눈에 띈다.
 */
function warnUnhandledActions() {
  const buttons = app.querySelectorAll('button');
  buttons.forEach((b) => {
    const handled = ACTIONS.some((a) => b.hasAttribute(a));
    if (!handled) {
      console.error(
        '클릭 처리기가 받지 못하는 버튼:',
        b.outerHTML.slice(0, 120),
      );
    }
  });
}

app.addEventListener('click', (e) => {
  const el = (e.target as HTMLElement).closest<HTMLElement>(ACTION_SELECTOR);
  if (!el) return;

  if (el.dataset.refresh) {
    void refreshDisks();
    return;
  }
  if (el.dataset.eject) {
    doEject();
    return;
  }
  if (el.dataset.cancel) {
    // 백엔드에 멈추라고 알린다. 화면 전환은 작업이 실제로 끝난 뒤
    // write_image 가 반환하면서 이뤄진다.
    if (!isBrowserPreview()) invoke('cancel_write').catch(() => {});
    else state.step = 3;
  } else if (el.dataset.disk) {
    state.selectedDisk = Number(el.dataset.disk);
  } else if (el.dataset.loader) {
    state.loader = el.dataset.loader as LoaderId;
  } else if (el.dataset.lang) {
    setLang(el.dataset.lang as Lang);
  } else if (el.dataset.go) {
    const next = Number(el.dataset.go) as Step;
    state.step = next;
    if (next === 4) startWrite();
    if (next === 1) {
      state.progress = null;
      state.failure = null;
      state.summary = null;
      state.eject = null;
    }
  }
  render();
});

/**
 * 작업을 시작한다.
 *
 * 백엔드가 진행 이벤트를 흘려보내고 화면은 그것만 그린다. 프런트엔드가
 * 진행률을 추측하지 않는 이유는, 추측한 값과 실제 상태가 어긋나면
 * 사용자가 다 끝난 줄 알고 USB 를 뽑기 때문이다.
 */
async function startWrite() {
  state.progress = null;
  state.failure = null;

  if (isBrowserPreview()) {
    simulateRun();
    return;
  }

  // 백엔드가 흘려보내는 진행 이벤트를 구독한다.
  // 작업이 끝나면 해제해서, 다시 실행할 때 리스너가 쌓이지 않게 한다.
  const unlisten = await listen<ProgressEvent>('progress', (e) => {
    state.progress = e.payload;
    if (state.step === 4) render();
  });

  try {
    state.summary = await invoke<RunSummary>('write_image', {
      diskNumber: state.selectedDisk,
      loader: state.loader,
      verify: state.verify,
    });
    state.step = 5;
  } catch (err) {
    state.failure = normalizeFailure(err);
    state.step = 6;
  } finally {
    unlisten();
  }
  render();
}

/**
 * USB 를 안전하게 제거한다.
 *
 * 자동으로 하지 않는다. 자동 꺼내기는 실패해도 사용자가 알 수 없고 다시 시도할
 * 방법도 없다. 눌러서 결과를 보는 편이 낫다.
 */
async function doEject() {
  if (state.eject === 'busy' || state.selectedDisk == null) return;
  state.eject = 'busy';
  render();

  if (isBrowserPreview()) {
    setTimeout(() => {
      state.eject = 'ok';
      render();
    }, 700);
    return;
  }

  try {
    await invoke('eject_disk', { diskNumber: state.selectedDisk });
    state.eject = 'ok';
  } catch {
    state.eject = 'fail';
  }
  render();
}

/** 브라우저 미리보기에서 진행 화면을 확인하기 위한 모의 실행. */
function simulateRun() {
  const stages = plannedStages();
  const sizes: Partial<Record<Stage, number>> = {
    Downloading: 605_888_202,
    Extracting: 3_026_190_336,
    Writing: 3_026_190_336,
    Verifying: 3_026_190_336,
  };
  const completed: Stage[] = [];
  let si = 0;
  let done = 0;

  const tick = () => {
    if (si >= stages.length) {
      state.summary = {
        loader: 'm-shell',
        tag: 'v1.4.2.8',
        asset_name: 'alpine-redpill.v1.4.2.8.m-shell-5GB.img.gz',
        bytes_written: 4_978_638_848,
        verified: state.verify,
      };
      state.step = 5;
      render();
      return;
    }
    const stage = stages[si];
    const total = sizes[stage] ?? null;
    const step = total ? total / 18 : 0;
    done += step;

    if (!total || done >= total) {
      completed.push(stage);
      si++;
      done = 0;
    }

    state.progress = {
      stage,
      percent: total ? Math.min(100, Math.floor((done / total) * 100)) : null,
      done_bytes: Math.floor(done),
      total_bytes: total,
      bytes_per_sec: total ? 42_000_000 : null,
      eta_secs: total ? Math.max(0, Math.floor((total - done) / 42_000_000)) : null,
      completed: [...completed],
      detail: null,
    };
    render();
    setTimeout(tick, total ? 140 : 500);
  };
  tick();
}

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

/**
 * 목록을 다시 읽는다.
 *
 * 겹쳐 호출되지 않게 막는다. 열거는 디스크 번호 64개를 훑고 볼륨까지 열거하므로
 * 싸지 않다.
 */
async function refreshDisks(): Promise<void> {
  if (state.scanning) return;
  state.scanning = true;
  render();
  try {
    const next = isBrowserPreview()
      ? previewDisks
      : await invoke<DiskEntry[]>('list_disks');
    state.disks = next;

    // 고른 USB 가 사라졌으면 선택을 지운다. 남겨두면 "다음" 이 눌리는데
    // 대상이 없는 상태가 된다.
    if (
      state.selectedDisk != null &&
      !next.some((d) => d.number === state.selectedDisk && d.ready)
    ) {
      state.selectedDisk = null;
    }
    // 쓸 수 있는 것이 하나뿐이면 미리 골라둔다.
    const ready = next.filter((d) => d.ready);
    if (state.selectedDisk == null && ready.length === 1) {
      state.selectedDisk = ready[0].number;
    }
  } catch (err) {
    console.error('목록 갱신 실패', err);
    state.disks = [];
  } finally {
    state.scanning = false;
    render();
  }
}

/**
 * 1단계에 머무는 동안 주기적으로 다시 읽는다.
 *
 * 빈 화면에 "USB 를 꽂으면 자동으로 나타납니다" 라고 적어두고 실제로는 시작할 때
 * 한 번만 읽고 있었다. 사용자가 USB 를 다시 꽂아도 앱을 껐다 켜야 보였다.
 * 적어둔 대로 동작하게 만든다.
 */
function startAutoScan() {
  setInterval(() => {
    if (state.step === 1 && !state.scanning) void refreshDisks();
  }, 3000);
}

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
  startAutoScan();
}

boot();
