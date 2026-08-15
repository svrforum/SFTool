/**
 * 한국어 / 영어 문구.
 *
 * 백엔드는 사유를 코드로만 넘긴다. 문장을 백엔드에서 만들면 언어 전환이
 * 불가능해지기 때문이다.
 */

export type Lang = 'ko' | 'en';

type Dict = Record<string, string>;

const ko: Dict = {
  simulated: '개발용 가짜 데이터입니다. 실제 USB가 아닙니다.',

  step1_title: 'USB를\n선택해 주세요',
  step1_lead: 'USB 저장장치만 표시됩니다. 내장 디스크는 목록에 나오지 않습니다.',
  step1_empty: '연결된 USB가 없습니다',
  step1_empty_hint: 'USB를 꽂으면 자동으로 나타납니다',

  step2_title: '어떤 로더를\n쓰시겠어요?',
  step2_lead: '최신 버전을 자동으로 받아옵니다.',
  mshell_sub: '처음이시라면 이걸 고르세요',
  rr_sub: 'RROrg/rr',
  recommended: '강추',

  step3_title: '데이터가\n모두 지워집니다',
  step3_lead: '아래 USB의 기존 내용은 복구할 수 없습니다.',
  step3_note:
    '쓰기가 끝난 뒤 윈도우가 "디스크를 포맷하세요" 라고 물어볼 수 있습니다. 정상이며 반드시 취소를 누르세요.',

  step4_title: 'USB를\n만들고 있어요',
  step4_lead: '완료될 때까지 USB를 뽑지 마세요.',

  // 단계 이름
  stage_Resolving: '최신 버전 확인',
  stage_Downloading: '내려받기',
  stage_Extracting: '압축 해제',
  stage_Preparing: 'USB 준비',
  stage_Writing: '이미지 쓰기',
  stage_Verifying: '검증',
  stage_Finishing: '마무리',

  eta_seconds: '약 {0}초 남음',
  eta_minutes: '약 {0}분 남음',
  eta_almost: '곧 완료',
  speed: '{0}/s',
  of_total: '{0} / {1}',

  error_title: '실패했습니다',
  err_needs_elevation: '관리자 권한이 필요합니다',
  err_needs_elevation_why: '프로그램을 마우스 오른쪽 버튼으로 눌러 "관리자 권한으로 실행"을 선택해 주세요.',
  err_locked: 'USB를 잠글 수 없습니다',
  err_locked_why: '다른 프로그램이 USB를 사용 중일 수 있습니다. 탐색기 창을 닫고 다시 시도해 주세요.',
  err_write_denied: '쓰기가 거부되었습니다',
  err_write_denied_why: 'Windows 보안의 "제어된 폴더 액세스"가 원인일 수 있습니다. 잠시 끄고 다시 시도해 보세요.',
  err_media_changed: 'USB가 바뀌었습니다',
  err_media_changed_why: '작업 중에 USB가 분리되었거나 교체되었습니다. 다시 꽂고 처음부터 진행해 주세요.',
  err_identity_changed: '다른 장치입니다',
  err_identity_changed_why: '선택한 USB와 실제 장치가 다릅니다. 안전을 위해 중단했습니다.',
  err_network: '내려받지 못했습니다',
  err_network_why: '네트워크 연결을 확인하고 다시 시도해 주세요.',
  err_generic_why: '문제가 계속되면 아래 내용을 함께 알려주세요.',
  retry: '다시 시도',

  done_title: 'USB가\n준비됐어요',
  done_lead:
    '이 USB로 부팅한 뒤, 로더 화면에서 시놀로지 모델을 선택하시면 됩니다.',

  next: '다음',
  back: '뒤로',
  erase_and_write: '지우고 쓰기',
  cancel: '취소',
  done: '완료',

  // 사용 불가 사유 (백엔드 코드 → 문구)
  reason_read_only: '쓰기 금지 상태입니다',
  reason_too_small_for_any_image: '용량이 부족합니다 (최소 {0} 필요)',
  reason_image_too_large: '이미지가 이 USB보다 큽니다 ({0} 필요)',
  reason_no_media: '미디어가 없습니다',
  reason_spanned_volume: '여러 디스크에 걸친 볼륨이 있습니다',
  reason_source_on_target: '이미지가 이 USB에 있습니다',
};

const en: Dict = {
  simulated: 'Showing simulated devices. These are not real USB drives.',

  step1_title: 'Choose\nyour USB drive',
  step1_lead:
    'Only USB storage is listed. Internal disks never appear here.',
  step1_empty: 'No USB drive connected',
  step1_empty_hint: 'Plug one in and it will show up automatically',

  step2_title: 'Which loader\nwould you like?',
  step2_lead: 'The latest release is fetched automatically.',
  mshell_sub: 'Pick this one if you are unsure',
  rr_sub: 'RROrg/rr',
  recommended: 'PICK',

  step3_title: 'Everything\nwill be erased',
  step3_lead: 'The current contents of this drive cannot be recovered.',
  step3_note:
    'Windows may ask you to format the disk once writing finishes. That is expected — always choose Cancel.',

  step4_title: 'Creating\nyour USB drive',
  step4_lead: 'Do not unplug the drive until this finishes.',

  stage_Resolving: 'Checking latest release',
  stage_Downloading: 'Downloading',
  stage_Extracting: 'Extracting',
  stage_Preparing: 'Preparing the drive',
  stage_Writing: 'Writing image',
  stage_Verifying: 'Verifying',
  stage_Finishing: 'Finishing up',

  eta_seconds: '{0}s left',
  eta_minutes: '{0} min left',
  eta_almost: 'almost done',
  speed: '{0}/s',
  of_total: '{0} / {1}',

  error_title: 'Something went wrong',
  err_needs_elevation: 'Administrator rights are required',
  err_needs_elevation_why:
    'Right-click the program and choose "Run as administrator".',
  err_locked: 'The drive could not be locked',
  err_locked_why:
    'Another program may be using it. Close any Explorer windows and try again.',
  err_write_denied: 'Writing was denied',
  err_write_denied_why:
    'Controlled folder access in Windows Security may be blocking this. Try turning it off briefly.',
  err_media_changed: 'The drive changed',
  err_media_changed_why:
    'It was unplugged or swapped during the operation. Reconnect it and start over.',
  err_identity_changed: 'This is a different device',
  err_identity_changed_why:
    'The drive no longer matches the one you selected. Stopped for safety.',
  err_network: 'Download failed',
  err_network_why: 'Check your network connection and try again.',
  err_generic_why: 'If this keeps happening, please include the details below.',
  retry: 'Try again',

  done_title: 'Your USB drive\nis ready',
  done_lead:
    'Boot from this drive, then choose your Synology model in the loader screen.',

  next: 'Next',
  back: 'Back',
  erase_and_write: 'Erase and write',
  cancel: 'Cancel',
  done: 'Done',

  reason_read_only: 'Write protected',
  reason_too_small_for_any_image: 'Not enough space ({0} minimum)',
  reason_image_too_large: 'The image is larger than this drive ({0} needed)',
  reason_no_media: 'No media inserted',
  reason_spanned_volume: 'Contains a volume spanning several disks',
  reason_source_on_target: 'The image lives on this drive',
};

const dicts: Record<Lang, Dict> = { ko, en };

let current: Lang = detect();

/** 시스템 언어로 초기값을 정한다. 한국어면 한국어, 아니면 영어. */
function detect(): Lang {
  const nav = navigator.language?.toLowerCase() ?? '';
  return nav.startsWith('ko') ? 'ko' : 'en';
}

export function getLang(): Lang {
  return current;
}

export function setLang(l: Lang) {
  current = l;
}

/** 문구를 가져온다. `{0}`, `{1}` 자리에 인자를 채운다. */
export function t(key: string, ...args: string[]): string {
  const raw = dicts[current][key] ?? dicts.en[key] ?? key;
  return raw.replace(/\{(\d+)\}/g, (m, i) => args[Number(i)] ?? m);
}

/** 백엔드가 준 사유 코드를 문구로 바꾼다. */
export function reasonText(code: string, detail?: string | null): string {
  return t(`reason_${code}`, detail ?? '');
}
