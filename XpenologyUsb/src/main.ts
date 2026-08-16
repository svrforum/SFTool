/**
 * 시작 화면에서 갈라지는 두 갈래 위저드.
 *
 * 화면당 결정 하나. 상태는 단순한 객체 하나로 두고 화면을 통째로 다시 그린다 —
 * 화면이 몇 개뿐이라 부분 갱신을 최적화할 이유가 없고, 전체 렌더가 상태와
 * 화면이 어긋나는 부류의 버그를 없앤다.
 *
 * 굽기와 복제를 한 화면에 섞지 않고 맨 앞에서 갈라내는 이유는, 섞으면 "대상을
 * 먼저 고르고 원본을 나중에 고르는" 순서가 나오기 때문이다. 어느 쪽이 지워지는지
 * 흐려지는 순간 사용자는 멀쩡한 USB 를 잃는다.
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
  cleanDetail,
  segs,
  type DiskEntry,
  type DiskList,
  type Failure,
  type ProgressEvent,
  type Stage,
} from './ui';
import './styles.css';

type LoaderId = 'MShell' | 'Rr';
type Step = 1 | 2 | 3 | 4 | 5 | 6;

/** 시작 화면에서 고르는 갈래. */
type Mode = 'home' | 'burn' | 'clone';

/**
 * 진행 화면에 나오는 단계 이름.
 *
 * 복제에는 굽기에 없는 `Analyzing` 이 있다. 두 흐름이 같은 진행 화면을 쓰므로
 * 여기서 합쳐 둔다.
 */
type FlowStage = Stage | 'Analyzing';

/** 한 화면. 껍데기(배너·언어 전환·footer)는 `render` 가 붙인다. */
type Screen = { body: string; foot: string };

type RunSummary = {
  loader: string;
  tag: string;
  asset_name: string;
  bytes_written: number;
  verified: boolean;
};

/** `analyze_source` 의 결과. 확인 화면이 복사할 양을 미리 보여주는 데 쓴다. */
type SourcePlan = {
  bytes: number;
  size_label: string;
  partitions: number;
  scheme: string;
};

type CloneSummary = {
  bytes_copied: number;
  partitions: number;
  verified: boolean;
  source_name: string;
  target_name: string;
};

type State = {
  mode: Mode;
  step: Step;
  disks: DiskEntry[];
  selectedDisk: number | null;
  /** 복제 원본 디스크 번호. */
  source: number | null;
  /** 복제 대상 디스크 번호. */
  target: number | null;
  /** 원본 분석 결과. 확인 화면에서 채워진다. */
  plan: SourcePlan | null;
  loader: LoaderId;
  verify: boolean;
  simulated: boolean;
  loading: boolean;
  progress: ProgressEvent | null;
  failure: Failure | null;
  summary: RunSummary | null;
  cloneSummary: CloneSummary | null;
  /** 안전 제거 상태. null 이면 아직 누르지 않은 것. */
  eject: 'busy' | 'ok' | 'fail' | null;
  /** 목록을 다시 읽는 중인가. */
  scanning: boolean;
  /**
   * 열거에서 빠진 장치와 그 사유, 또는 열거 자체가 실패한 이유.
   *
   * 목록 밑에 그대로 보여준다. 이게 없던 시절에는 사용자의 USB 가 조회 실패로
   * 빠져도 화면에는 "연결된 USB가 없습니다" 만 떴다 — 원인은 백엔드 안에만
   * 있었고, 사용자가 할 수 있는 일은 USB 를 다시 꽂아보는 것뿐이었다.
   */
  diskNotes: string[];
};

const state: State = {
  mode: 'home',
  step: 1,
  disks: [],
  selectedDisk: null,
  source: null,
  target: null,
  plan: null,
  loader: 'MShell',
  verify: false,
  simulated: false,
  loading: true,
  progress: null,
  failure: null,
  summary: null,
  cloneSummary: null,
  eject: null,
  scanning: false,
  diskNotes: [],
};

/** 굽기의 단계 순서. 검증은 선택이라 켰을 때만 들어간다. */
function plannedStages(): FlowStage[] {
  const s: FlowStage[] = [
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

/** 복제의 단계 순서. 내려받기와 압축 해제가 없고, 대신 원본 분석이 있다. */
function cloneStages(): FlowStage[] {
  const s: FlowStage[] = ['Analyzing', 'Preparing', 'Writing'];
  if (state.verify) s.push('Verifying');
  s.push('Finishing');
  return s;
}

const app = document.querySelector<HTMLDivElement>('#app')!;

/** 선택된 디스크. 목록이 갱신되며 사라졌을 수 있으므로 매번 조회한다. */
function selected(): DiskEntry | undefined {
  return state.disks.find((d) => d.number === state.selectedDisk);
}

/** 지금 흐름에서 안전 제거의 대상. 복제는 갓 만들어진 대상 USB 다. */
function ejectTarget(): number | null {
  return state.mode === 'clone' ? state.target : state.selectedDisk;
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

/** 디스크 목록. 비어 있으면 "꽂으면 나타납니다" 안내로 바꾼다. */
function diskList(disks: DiskEntry[], selectedNumber: number | null): string {
  // 빠진 장치의 사유는 목록이 비었든 아니든 보여준다. 하나만 빠진 경우가
  // 오히려 알기 어렵다 — 목록에 다른 것이 있으니 아무 문제가 없어 보인다.
  const notes = state.diskNotes.length
    ? `<div class="hint scan-notes">${esc(t('step1_skipped'))}
         ${state.diskNotes.map((n) => `<div>${esc(n)}</div>`).join('')}
       </div>`
    : '';
  if (disks.length === 0) {
    return `<div class="empty">${esc(t('step1_empty'))}
              <div class="hint">${esc(t('step1_empty_hint'))}</div>
              ${notes}
            </div>`;
  }
  return disks.map((d) => diskItem(d, selectedNumber)).join('') + notes;
}

/** 시작 화면. 여기서만 갈래를 고를 수 있다. */
function homeScreen(): string {
  return `
    <main class="home">
      <div class="center">
        <h1>${nl(t('app_title'))}</h1>
        <div class="modes">
          <button class="mode" data-mode="burn">
            <span class="mode-ico">💾</span>
            <span class="mode-name">${esc(t('mode_burn_title'))}</span>
            <span class="mode-sub">${esc(t('mode_burn_sub'))}</span>
          </button>
          <button class="mode" data-mode="clone">
            <span class="mode-ico">⧉</span>
            <span class="mode-name">${esc(t('mode_clone_title'))}</span>
            <span class="mode-sub">${esc(t('mode_clone_sub'))}</span>
          </button>
        </div>
      </div>
    </main>`;
}

/**
 * 진행 화면. 굽기와 복제가 함께 쓴다 — 단계 목록만 다르다.
 *
 * 사용자가 "지금 뭐 하는 중인지" 한눈에 보게 하는 것이 목적이다.
 */
function progressScreen(stages: FlowStage[]): string {
  const p = state.progress;
  // 백엔드가 보낸 완료 목록. 흐름마다 단계 종류가 달라 이름으로만 대조한다.
  const completed: string[] = p?.completed ?? [];

  const list = stages
    .map((s) => {
      const done = completed.includes(s);
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

  return `
    ${segs(4)}
    <main>
      <div class="eyebrow">4 / 4</div>
      <h1>${nl(t('step4_title'))}</h1>
      <p class="lead">${esc(t('step4_lead'))}</p>
      <div class="stages">${list}</div>
      ${bar}
    </main>`;
}

/**
 * 완료 화면. 굽기와 복제가 함께 쓴다.
 *
 * `extra` 는 무엇이 얼마나 쓰였는지. "성공했습니다" 한 줄만 있으면, 탐색기에서
 * USB 내용이 보이지 않는 것을 보고 실패했다고 판단하게 된다.
 *
 * `diskNumber` 가 null 이면 안전 제거를 내놓지 않는다 — 어떤 장치를 꺼낼지
 * 모르는 상태에서 버튼만 있으면 눌러도 아무 일이 없다.
 */
function doneScreen(
  title: string,
  sub: string,
  extra: string,
  diskNumber: number | null,
): string {
  return `
    ${segs(4)}
    <main>
      <div class="center">
        <div class="tick">✓</div>
        <h1>${nl(title)}</h1>
        <p class="lead">${esc(sub)}</p>
        ${extra}
        ${diskNumber == null ? '' : ejectBlock(state.eject)}
      </div>
      <div class="note explain">
        <span>ℹ</span>
        <span><b>${esc(t('done_explorer_title'))}</b><br>${esc(
          t('done_explorer_body'),
        )}<br><span class="dim">${esc(t('done_replug'))}</span></span>
      </div>
    </main>`;
}

/** 실패 화면. 원인마다 다음에 뭘 해야 하는지 함께 알려준다. */
function errorScreen(): string {
  const f = state.failure;
  const code = f?.code ?? 'generic';
  const what = t(`err_${code}`) === `err_${code}` ? t('error_title') : t(`err_${code}`);
  const why =
    t(`err_${code}_why`) === `err_${code}_why`
      ? t('err_generic_why')
      : t(`err_${code}_why`);
  return `
    ${segs(4)}
    <main>
      <h1 class="danger">${nl(t('error_title'))}</h1>
      <div class="error-box">
        <div class="what">${esc(what)}</div>
        <div class="why">${esc(why)}</div>
        ${f?.detail ? `<div class="why">${esc(f.detail)}</div>` : ''}
      </div>
    </main>`;
}

/** 로더 굽기 흐름. */
function burnScreen(): Screen {
  if (state.step === 1) {
    return {
      body: `
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
        <div class="list">${diskList(state.disks, state.selectedDisk)}</div>
      </main>`,
      foot: `<button class="ghost" data-back="1">${esc(t('back'))}</button>
             <button class="cta" data-go="2" ${
               selected()?.ready ? '' : 'disabled'
             }>${esc(t('next'))}</button>`,
    };
  }
  if (state.step === 2) {
    return {
      body: `
      ${segs(2)}
      <main>
        <div class="eyebrow">2 / 4</div>
        <h1>${nl(t('step2_title'))}</h1>
        <p class="lead">${esc(t('step2_lead'))}</p>
        <div class="list">
          ${loaderItem('MShell', 'm-shell', t('mshell_sub'), true)}
          ${loaderItem('Rr', 'RR', t('rr_sub'), false)}
        </div>
      </main>`,
      foot: `<button class="ghost" data-go="1">${esc(t('back'))}</button>
             <button class="cta" data-go="3">${esc(t('next'))}</button>`,
    };
  }
  if (state.step === 3) {
    const d = selected();
    return {
      body: `
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
      </main>`,
      foot: `<button class="ghost" data-go="2">${esc(t('back'))}</button>
             <button class="cta danger" data-go="4">${esc(
               t('erase_and_write'),
             )}</button>`,
    };
  }
  if (state.step === 4) {
    // 취소는 화면만 되돌리는 것이 아니라 백엔드 작업을 실제로 멈춰야 한다.
    // 쓰기가 계속 도는데 화면만 3단계로 가면 사용자가 USB 를 뽑는다.
    return {
      body: progressScreen(plannedStages()),
      foot: `<button class="ghost" data-cancel="1">${esc(t('cancel'))}</button>`,
    };
  }
  if (state.step === 6) {
    return {
      body: errorScreen(),
      foot: `<button class="ghost" data-go="1">${esc(t('back'))}</button>
             <button class="cta" data-go="3">${esc(t('retry'))}</button>`,
    };
  }

  const sm = state.summary;
  const written = sm
    ? `<div class="written">${esc(
        t('done_written', sm.loader, sm.tag, fmtBytes(sm.bytes_written)),
      )}</div>`
    : '';
  const verified = sm?.verified
    ? `<div class="written ok">✓ ${esc(t('done_verified'))}</div>`
    : '';
  return {
    body: doneScreen(
      t('done_title'),
      t('done_lead'),
      `${written}${verified}`,
      state.selectedDisk,
    ),
    foot: `<button class="cta" data-mode="home">${esc(t('done'))}</button>`,
  };
}

/** USB 복제 흐름. 1=원본, 2=대상, 3=확인, 4=복사, 5=완료, 6=실패. */
function cloneScreen(): Screen {
  if (state.step === 1) {
    return {
      body: `
      ${segs(1)}
      <main>
        <div class="eyebrow">1 / 4</div>
        <div class="row-head">
          <h1>${nl(t('clone_pick_source'))}</h1>
          <button class="refresh" data-refresh="1" ${
            state.scanning ? 'disabled' : ''
          } title="${esc(t('refresh'))}">${state.scanning ? '⋯' : '↻'}</button>
        </div>
        <p class="lead">${esc(t('clone_pick_source_hint'))}</p>
        <div class="list">${diskList(state.disks, state.source)}</div>
      </main>`,
      foot: `<button class="ghost" data-back="1">${esc(t('back'))}</button>`,
    };
  }
  if (state.step === 2) {
    // 원본은 대상 목록에서 뺀다. 고를 수 있게 두면 확인 화면까지 가서야 막힌다.
    const choices = state.disks.filter((d) => d.number !== state.source);
    return {
      body: `
      ${segs(2)}
      <main>
        <div class="eyebrow">2 / 4</div>
        <h1 class="danger">${nl(t('clone_pick_target'))}</h1>
        <p class="lead">${esc(t('clone_pick_target_hint'))}</p>
        <div class="list">${diskList(choices, state.target)}</div>
      </main>`,
      foot: `<button class="ghost" data-back="1">${esc(t('back'))}</button>`,
    };
  }
  if (state.step === 3) {
    const src = state.disks.find((d) => d.number === state.source);
    const dst = state.disks.find((d) => d.number === state.target);
    // 분석이 끝나기 전에는 무엇을 얼마나 옮기는지 모른다. 그 사이에도 어느 쪽이
    // 지워지는지는 이미 보여 준다 — 기다리는 동안 방향을 확인하게 된다.
    const plan = state.plan;
    return {
      body: `
      ${segs(3)}
      <main>
        <div class="eyebrow">3 / 4</div>
        <h1 class="danger">${nl(t('clone_confirm_title'))}</h1>
        <div class="clone-confirm">
          <div class="side">
            <span class="side-label">${esc(t('clone_source'))}</span>
            <strong>${esc(src?.name ?? '')}</strong>
            <span class="side-note">${
              plan
                ? `${esc(plan.scheme)} · ${esc(
                    t('clone_partitions', String(plan.partitions)),
                  )}`
                : esc(t('clone_analyzing'))
            }</span>
            <span class="side-note">${
              plan ? `${esc(t('clone_amount'))} ${esc(plan.size_label)}` : ''
            }</span>
          </div>
          <div class="arrow">→</div>
          <div class="side danger">
            <span class="side-label">${esc(t('clone_target'))}</span>
            <strong>${esc(dst?.name ?? '')}</strong>
            <span class="side-note warn">⚠ ${esc(t('clone_pick_target_hint'))}</span>
          </div>
        </div>
        <label class="check">
          <input type="checkbox" data-verify="1" ${state.verify ? 'checked' : ''}>
          <span>${esc(t('verify_label'))}</span>
        </label>
      </main>`,
      foot: `<button class="ghost" data-back="1">${esc(t('back'))}</button>
             <button class="cta danger" data-go="4" ${plan ? '' : 'disabled'}>${esc(
               t('clone_go'),
             )}</button>`,
    };
  }
  if (state.step === 4) {
    return {
      body: progressScreen(cloneStages()),
      foot: `<button class="ghost" data-cancel="1">${esc(t('cancel'))}</button>`,
    };
  }
  if (state.step === 6) {
    // 다시 시도는 원본 선택부터다. 분석에서 실패했다면 확인 화면으로 돌아가도
    // 다시 분석할 계기가 없어 "살펴보는 중" 에 멈춘 화면만 보인다.
    return {
      body: errorScreen(),
      foot: `<button class="ghost" data-mode="home">${esc(t('back'))}</button>
             <button class="cta" data-mode="clone">${esc(t('retry'))}</button>`,
    };
  }

  const cs = state.cloneSummary;
  const copied = cs
    ? `<div class="written">${esc(t('clone_amount'))} ${esc(
        fmtBytes(cs.bytes_copied),
      )} · ${esc(t('clone_partitions', String(cs.partitions)))}</div>`
    : '';
  const verified = cs?.verified
    ? `<div class="written ok">✓ ${esc(t('done_verified'))}</div>`
    : '';
  return {
    body: doneScreen(
      t('clone_done'),
      t(
        'clone_done_sub',
        cs?.source_name ?? '',
        cs?.target_name ?? '',
      ),
      `${copied}${verified}`,
      state.target,
    ),
    foot: `<button class="cta" data-mode="home">${esc(t('done'))}</button>`,
  };
}

function render() {
  const banner = state.simulated
    ? `<div class="sim-banner">${esc(t('simulated'))}</div>`
    : '';
  const lang = `<div class="lang">
      <button data-lang="ko" aria-pressed="${getLang() === 'ko'}">한국어</button>
      <button data-lang="en" aria-pressed="${getLang() === 'en'}">EN</button>
    </div>`;

  // 시작 화면에는 단계 막대도 하단 버튼도 없다.
  if (state.mode === 'home') {
    app.innerHTML = `${banner}${lang}${homeScreen()}`;
    warnUnhandledActions();
    return;
  }

  const { body, foot } = state.mode === 'clone' ? cloneScreen() : burnScreen();

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
    void doEject();
    return;
  }
  if (el.dataset.mode) {
    // 갈래를 바꾸면 이전 흐름이 남긴 것을 전부 버린다. 남겨두면 완료 화면의
    // 요약이나 고른 USB 가 다음 흐름에 그대로 따라 들어온다.
    startMode(el.dataset.mode as Mode);
    return;
  }
  if (el.dataset.back) {
    // 첫 단계에서의 「뒤로」는 시작 화면이다.
    if (state.step === 1) startMode('home');
    else {
      state.step = (state.step - 1) as Step;
      render();
    }
    return;
  }

  if (el.dataset.cancel) {
    // 백엔드에 멈추라고 알린다. 화면 전환은 작업이 실제로 끝난 뒤
    // write_image / clone_disk 가 반환하면서 이뤄진다.
    if (!isBrowserPreview()) invoke('cancel_write').catch(() => {});
    else state.step = 3;
  } else if (el.dataset.disk) {
    const n = Number(el.dataset.disk);
    if (state.mode === 'clone') {
      // 복제는 고르는 즉시 다음 단계로 간다. pickCloneDisk 가 직접 그린다.
      void pickCloneDisk(n);
      return;
    }
    state.selectedDisk = n;
  } else if (el.dataset.loader) {
    state.loader = el.dataset.loader as LoaderId;
  } else if (el.dataset.lang) {
    setLang(el.dataset.lang as Lang);
  } else if (el.dataset.go) {
    const next = Number(el.dataset.go) as Step;
    state.step = next;
    if (next === 4) {
      if (state.mode === 'clone') void startClone();
      else void startWrite();
    }
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
 * 검증 체크박스.
 *
 * 클릭 위임 목록에 넣지 않는다. 넣으면 누를 때마다 화면을 다시 그리게 되고,
 * 브라우저가 이미 바꿔 놓은 체크 표시를 우리가 덮어쓰는 모양이 된다.
 */
app.addEventListener('change', (e) => {
  const el = e.target;
  if (el instanceof HTMLInputElement && el.hasAttribute('data-verify')) {
    state.verify = el.checked;
  }
});

/** 갈래를 시작한다. 시작 화면으로 돌아가는 것도 여기를 지난다. */
function startMode(mode: Mode) {
  state.mode = mode;
  state.step = 1;
  // 검증 체크박스는 복제 확인 화면에만 있다. 켠 채로 굽기로 넘어가면 사용자가
  // 보지도 끄지도 못하는 검증이 붙어 시간만 두 배가 된다.
  state.verify = false;
  state.selectedDisk = null;
  state.source = null;
  state.target = null;
  state.plan = null;
  state.progress = null;
  state.failure = null;
  state.summary = null;
  state.cloneSummary = null;
  state.eject = null;
  render();
  if (mode !== 'home') void refreshDisks();
}

/**
 * 복제에서 디스크를 고른다.
 *
 * 대상을 고르는 순간 원본 분석을 시작한다. 확인 화면은 "복사할 양" 을 보여줘야
 * 하는데, 그 값은 원본을 실제로 읽어야 나온다. 대상은 아직 건드리지 않는다.
 */
async function pickCloneDisk(n: number) {
  if (state.step === 1) {
    state.source = n;
    state.step = 2;
    render();
    return;
  }

  state.target = n;
  state.step = 3;
  state.plan = null;
  render(); // 먼저 "원본을 살펴보는 중" 을 보여준다

  try {
    state.plan = isBrowserPreview()
      ? previewPlan
      : await invoke<SourcePlan>('analyze_source', { diskNumber: state.source });
  } catch (err) {
    state.failure = cloneFailure(err);
    state.step = 6;
  }
  render();
}

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
    simulate(plannedStages(), () => {
      state.summary = {
        loader: 'm-shell',
        tag: 'v1.4.2.8',
        asset_name: 'alpine-redpill.v1.4.2.8.m-shell-5GB.img.gz',
        bytes_written: 4_978_638_848,
        verified: state.verify,
      };
    });
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

/** 복제를 시작한다. 진행 이벤트는 굽기와 같은 통로로 온다. */
async function startClone() {
  state.progress = null;
  state.failure = null;

  if (isBrowserPreview()) {
    simulate(cloneStages(), () => {
      state.cloneSummary = {
        bytes_copied: state.plan?.bytes ?? 0,
        partitions: state.plan?.partitions ?? 0,
        verified: state.verify,
        source_name:
          state.disks.find((d) => d.number === state.source)?.name ?? '',
        target_name:
          state.disks.find((d) => d.number === state.target)?.name ?? '',
      };
    });
    return;
  }

  const unlisten = await listen<ProgressEvent>('progress', (e) => {
    state.progress = e.payload;
    if (state.step === 4) render();
  });

  try {
    state.cloneSummary = await invoke<CloneSummary>('clone_disk', {
      source: state.source,
      target: state.target,
      verify: state.verify,
    });
    state.step = 5;
  } catch (err) {
    state.failure = cloneFailure(err);
    state.step = 6;
  } finally {
    unlisten();
  }
  render();
}

/**
 * 복제에만 나오는 실패를 문구가 있는 코드로 바꾼다.
 *
 * 백엔드는 `Layout(Gpt)` 같은 Rust 디버그 문자열을 그대로 넘긴다. 그대로
 * 보여주면 사용자가 다음에 무엇을 해야 할지 알 수 없으므로, 대응할 방법이
 * 있는 것만 골라낸다. 나머지는 굽기와 같은 해석기에 맡긴다.
 */
function cloneFailure(err: unknown): Failure {
  const s = typeof err === 'string' ? err : JSON.stringify(err);
  if (s.includes('Gpt')) return { code: 'layout_gpt' };
  if (s.includes('NoSignature') || s.includes('NoPartitions')) {
    return { code: 'layout_nosig' };
  }
  if (s.includes('SameDisk')) return { code: 'same_disk' };
  return normalizeFailure(err);
}

/**
 * USB 를 안전하게 제거한다.
 *
 * 자동으로 하지 않는다. 자동 꺼내기는 실패해도 사용자가 알 수 없고 다시 시도할
 * 방법도 없다. 눌러서 결과를 보는 편이 낫다.
 */
async function doEject() {
  const diskNumber = ejectTarget();
  if (state.eject === 'busy' || diskNumber == null) return;
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
    await invoke('eject_disk', { diskNumber });
    state.eject = 'ok';
  } catch {
    state.eject = 'fail';
  }
  render();
}

/**
 * 브라우저 미리보기에서 진행 화면을 확인하기 위한 모의 실행.
 *
 * `finish` 는 마지막에 요약을 채운다 — 흐름마다 요약의 모양이 다르다.
 */
function simulate(stages: FlowStage[], finish: () => void) {
  const sizes: Partial<Record<FlowStage, number>> = {
    Downloading: 605_888_202,
    Extracting: 3_026_190_336,
    Writing: 3_026_190_336,
    Verifying: 3_026_190_336,
  };
  const completed: FlowStage[] = [];
  let si = 0;
  let done = 0;

  const tick = () => {
    if (si >= stages.length) {
      finish();
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
      // 화면은 단계를 이름으로만 다루므로 굽기에 없는 단계도 그대로 넣는다.
      stage: stage as Stage,
      percent: total ? Math.min(100, Math.floor((done / total) * 100)) : null,
      done_bytes: Math.floor(done),
      total_bytes: total,
      bytes_per_sec: total ? 42_000_000 : null,
      eta_secs: total ? Math.max(0, Math.floor((total - done) / 42_000_000)) : null,
      completed: [...completed] as Stage[],
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

/** 브라우저 미리보기용 분석 결과. 32GB USB 에 5GB 로더가 들어 있는 경우. */
const previewPlan: SourcePlan = {
  bytes: 4_978_638_848,
  size_label: '4.98 GB',
  partitions: 3,
  scheme: 'MBR',
};

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
    const listed = isBrowserPreview()
      ? { disks: previewDisks, notes: [] }
      : await invoke<DiskList>('list_disks');
    const next = listed.disks;
    state.disks = next;
    state.diskNotes = listed.notes;

    // 고른 USB 가 사라졌으면 선택을 지운다. 남겨두면 "다음" 이 눌리는데
    // 대상이 없는 상태가 된다.
    if (
      state.selectedDisk != null &&
      !next.some((d) => d.number === state.selectedDisk && d.ready)
    ) {
      state.selectedDisk = null;
    }
    // 복제 쪽도 마찬가지다. 뽑힌 USB 를 원본으로 들고 있으면 확인 화면에
    // 이름 없는 상자가 남는다.
    if (state.source != null && !next.some((d) => d.number === state.source)) {
      state.source = null;
    }
    if (state.target != null && !next.some((d) => d.number === state.target)) {
      state.target = null;
    }
    // 쓸 수 있는 것이 하나뿐이면 미리 골라둔다.
    const ready = next.filter((d) => d.ready);
    if (state.selectedDisk == null && ready.length === 1) {
      state.selectedDisk = ready[0].number;
    }
  } catch (err) {
    // 콘솔에만 적고 빈 목록을 보여주면, 사용자는 USB 가 없다고 읽는다.
    // 열거가 실패한 것과 USB 가 없는 것은 다른 상황이고 할 일도 다르다.
    console.error('목록 갱신 실패', err);
    state.disks = [];
    state.diskNotes = [cleanDetail(String(err))];
  } finally {
    state.scanning = false;
    render();
  }
}

/**
 * 목록을 고르는 동안 주기적으로 다시 읽는다.
 *
 * 빈 화면에 "USB 를 꽂으면 자동으로 나타납니다" 라고 적어두고 실제로는 시작할 때
 * 한 번만 읽고 있었다. 사용자가 USB 를 다시 꽂아도 앱을 껐다 켜야 보였다.
 * 적어둔 대로 동작하게 만든다.
 */
function startAutoScan() {
  setInterval(() => {
    if (state.mode === 'home' || state.scanning) return;
    // 복제는 2단계에서도 목록을 보여준다.
    const listing = state.step === 1 || (state.mode === 'clone' && state.step === 2);
    if (listing) void refreshDisks();
  }, 3000);
}

async function boot() {
  try {
    if (isBrowserPreview()) {
      state.simulated = true;
      state.disks = previewDisks;
    } else {
      state.simulated = await invoke<boolean>('is_simulated');
      const listed = await invoke<DiskList>('list_disks');
      state.disks = listed.disks;
      state.diskNotes = listed.notes;
    }
    // 선택 가능한 것이 하나뿐이면 미리 골라둔다. 흔한 경우라 클릭을 아낀다.
    const ready = state.disks.filter((d) => d.ready);
    if (ready.length === 1) state.selectedDisk = ready[0].number;
  } catch (err) {
    console.error('열거 실패', err);
    state.diskNotes = [cleanDetail(String(err))];
  } finally {
    state.loading = false;
    render();
  }
  startAutoScan();
}

boot();
