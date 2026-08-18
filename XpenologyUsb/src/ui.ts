/**
 * 두 흐름이 함께 쓰는 화면 조각.
 *
 * 로더 굽기와 USB 복제는 고르는 대상만 다를 뿐, 디스크 목록·진행률 표시·오류
 * 해석이 똑같다. 한쪽 흐름 안에 두면 다른 쪽이 베껴 쓰게 되고 그때부터 둘이
 * 조금씩 어긋난다.
 *
 * 여기 있는 것들은 상태 객체를 보지 않고 인자만 본다. 그래야 어느 흐름에서
 * 불러도 같은 결과가 나온다.
 */

import { reasonText, t } from './i18n';

export type DiskEntry = {
  number: number;
  name: string;
  size_bytes: number;
  size_label: string;
  drive_letters: string[];
  ready: boolean;
  blocked_reason: string | null;
  blocked_detail: string | null;
};

/** `list_disks` 가 돌려주는 것. 목록과, 열거에서 빠진 것들의 사유. */
export type DiskList = {
  disks: DiskEntry[];
  notes: string[];
};

export type Stage =
  | 'Resolving'
  | 'Downloading'
  | 'Extracting'
  | 'Preparing'
  | 'Writing'
  | 'Verifying'
  | 'Finishing';

export type ProgressEvent = {
  stage: Stage;
  percent: number | null;
  done_bytes: number;
  total_bytes: number | null;
  bytes_per_sec: number | null;
  eta_secs: number | null;
  completed: Stage[];
  detail: string | null;
};

export type Failure = { code: string; detail?: string };

export function fmtBytes(n: number): string {
  const u = ['B', 'KB', 'MB', 'GB', 'TB'];
  if (n < 1000) return `${n} B`;
  let v = n;
  let i = 0;
  while (v >= 1000 && i < u.length - 1) {
    v /= 1000;
    i++;
  }
  return `${v >= 100 ? v.toFixed(0) : v >= 10 ? v.toFixed(1) : v.toFixed(2)} ${u[i]}`;
}

export function fmtEta(secs: number): string {
  if (secs < 5) return t('eta_almost');
  if (secs < 60) return t('eta_seconds', String(secs));
  return t('eta_minutes', String(Math.ceil(secs / 60)));
}

/**
 * 클릭을 받는 data 속성들.
 *
 * **버튼을 추가할 때 여기에도 넣어야 한다.** 안전 제거 버튼과 새로고침 버튼이
 * 이 목록에서 빠져 있었고, 그래서 눌러도 아무 반응이 없었다 — 처리 코드는
 * 멀쩡히 있는데 `closest()` 가 요소를 찾지 못해 도달하지 못했다.
 * `main.ts` 의 개발용 검사가 렌더된 화면과 이 목록을 대조해 누락을 잡는다.
 */
export const ACTIONS = [
  'data-disk',
  'data-loader',
  'data-go',
  'data-lang',
  'data-cancel',
  'data-eject',
  'data-refresh',
  'data-mode',
  'data-src',
  'data-dst',
  'data-back',
] as const;

export const ACTION_SELECTOR = ACTIONS.map((a) => `[${a}]`).join(',');

export function esc(s: string): string {
  return s.replace(
    /[&<>"']/g,
    (c) =>
      ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[
        c
      ]!,
  );
}

/** 줄바꿈을 <br> 로. 제목에만 쓴다. */
export function nl(s: string): string {
  return esc(s).replace(/\n/g, '<br>');
}

/** `total` 은 단계 수. 흐름마다 다르다. */
export function segs(active: number, total = 4): string {
  return `<div class="steps">${Array.from({ length: total }, (_, i) => i + 1)
    .map((i) => `<i class="seg${i <= active ? ' on' : ''}"></i>`)
    .join('')}</div>`;
}

/** `selectedNumber` 는 지금 선택된 디스크 번호. 흐름마다 다른 값을 넘긴다. */
export function diskItem(d: DiskEntry, selectedNumber: number | null): string {
  const letters = d.drive_letters.length ? ` · ${d.drive_letters.join(' ')}` : '';
  const sub = d.ready
    ? `${esc(d.size_label)}${esc(letters)}`
    : reasonText(d.blocked_reason ?? '', d.blocked_detail);
  return `
    <button class="item" data-disk="${d.number}" ${d.ready ? '' : 'disabled'}
            aria-selected="${selectedNumber === d.number}">
      <span class="body">
        <span class="title">${esc(d.name)}</span>
        <span class="sub${d.ready ? '' : ' warn'}">${esc(sub)}</span>
      </span>
      <span class="radio"></span>
    </button>`;
}

/** 안전 제거 상태. null 이면 아직 누르지 않은 것. */
export type EjectStatus = 'busy' | 'ok' | 'fail' | null;

/** 완료 화면의 안전 제거 영역. */
export function ejectBlock(status: EjectStatus): string {
  if (status === 'ok') {
    return `<div class="written ok">✓ ${esc(t('eject_ok'))}</div>`;
  }
  if (status === 'fail') {
    return `<div class="eject-fail">
        <b>${esc(t('eject_fail'))}</b><br>${esc(t('eject_fail_why'))}
        <div><button class="mini" data-eject="1">${esc(t('eject'))}</button></div>
      </div>`;
  }
  const busy = status === 'busy';
  return `<div><button class="mini" data-eject="1" ${busy ? 'disabled' : ''}>${esc(
    busy ? t('ejecting') : t('eject'),
  )}</button></div>`;
}

/**
 * 백엔드 오류를 UI 가 아는 코드로 바꾼다.
 *
 * 원문 메시지는 **항상 함께 보여준다.** 예전에는 코드가 매칭되면 원문을
 * 버렸는데, 백엔드가 거기에 "볼륨 2/2 잠금, 파티션 테이블 초기화 실패" 같은
 * 진짜 원인을 담아 보내고 있었다. 친절한 제목만 남기고 그걸 버리면
 * 사용자도 나도 원인을 알 수 없다.
 */
export function normalizeFailure(err: unknown): Failure {
  const s = typeof err === 'string' ? err : JSON.stringify(err);
  // **순서가 의미를 가진다.** 위에 있는 것이 이긴다.
  //
  // `TargetErased` 가 맨 앞인 이유: 그 오류는 안에 원인을 그대로 품고 있어서
  // 문자열에 `Locked` 같은 이름이 함께 들어 있다. 뒤에 두면 "USB를 잠글 수
  // 없습니다 / 탐색기를 닫고 다시 시도" 가 이기는데, 그 안내는 USB 가
  // 멀쩡하다는 전제에서만 맞다. 이미 비워진 USB 를 두고 할 말이 아니다.
  const map: Record<string, string> = {
    // **`TargetErased` 보다 위에 있어야 한다.** 검증 실패도 되돌릴 수 없는
    // 지점 이후라 `TargetErased` 로 감싸여 오는데, 아래에 두면 "USB를
    // 준비하다 중단됐습니다" 가 이긴다. 그건 틀린 말이다 — 이미지는 끝까지
    // 쓰였고 멈춘 곳은 준비 단계가 아니라 대조다. 사용자가 할 일도 정반대다:
    // 되꽂아 이어서 하는 게 아니라 그 USB 를 믿지 않는 것이다.
    VerifyMismatch: 'verify_mismatch',
    TargetErased: 'target_erased',
    NeedsElevation: 'needs_elevation',
    Locked: 'locked',
    WriteDenied: 'write_denied',
    MediaChanged: 'media_changed',
    IdentityChanged: 'identity_changed',
  };

  // 백엔드가 코드 5 로 감싼 쓰기 거부. 준비 상태가 메시지에 들어 있다.
  if (s.includes('쓰기를 거부') || s.includes('refused the write')) {
    return { code: 'write_denied', detail: cleanDetail(s) };
  }
  for (const [k, v] of Object.entries(map)) {
    if (s.includes(k)) return { code: v, detail: cleanDetail(s) };
  }
  return { code: 'generic', detail: cleanDetail(s) };
}

/** Rust 디버그 표현에서 사람이 읽을 부분만 남긴다. */
export function cleanDetail(s: string): string {
  // Device(Io { code: 5, message: "..." }) 형태에서 message 만 꺼낸다.
  const m = s.match(/message:\s*"((?:[^"\\]|\\.)*)"/);
  if (m) return m[1].replace(/\\n/g, '\n').replace(/\\"/g, '"').slice(0, 600);

  // 사람에게 할 말이 없는 순수 디버그 표현은 아예 내보내지 않는다.
  //
  // `TargetErased { cause: VerifyMismatch }` 가 그대로 화면에 찍혀 나갔다.
  // 백엔드가 오류를 `format!("{e:?}")` 로 넘기기 때문에, 안에 문장이 없는
  // 변형은 타입 이름만 남는다. 그건 사용자에게 아무 정보가 아니면서 번역도
  // 되지 않고, 프로그램이 자기 내부를 흘리고 있다는 인상만 준다. 위의 친절한
  // 문구가 이미 같은 내용을 말하고 있으므로 여기서는 비워 둔다.
  if (/^[A-Za-z0-9_]+(\s*\{[^"]*\})?$/.test(s.trim())) return '';

  return s.slice(0, 600);
}
