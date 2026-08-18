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

  app_title: 'Xpenology USB',
  mode_burn_title: '로더 굽기',
  mode_burn_sub: '최신 로더를 받아 새 USB 에 굽습니다',
  mode_clone_title: 'USB 복제',
  mode_clone_sub: '이미 만든 USB 를 다른 USB 로 복사합니다',

  clone_pick_source: '복사할 원본\nUSB 를 고르세요',
  clone_pick_source_hint:
    '로더가 이미 써진 USB 입니다. 읽기만 하고 바꾸지 않습니다.',
  clone_pick_target: '복사해 넣을\nUSB 를 고르세요',
  clone_pick_target_hint: '이 USB 의 내용은 모두 사라집니다.',
  clone_confirm_title: '이 방향이\n맞나요?',
  clone_source: '원본 (읽기만)',
  clone_target: '대상 (전부 지워짐)',
  clone_amount: '복사할 양',
  clone_partitions: '파티션 {0}개',
  clone_go: '복제 시작',
  clone_analyzing: '원본을 살펴보는 중…',
  clone_done: '복제가\n끝났습니다',
  clone_done_sub: '{0} 를 {1} 로 복사했습니다',
  verify_label: '쓴 내용을 되읽어 대조합니다. 끄면 절반쯤 빨라지지만, USB가 제대로 받았는지는 부팅해 봐야 압니다',

  step1_title: 'USB를\n선택해 주세요',
  step1_lead: 'USB 저장장치만 표시됩니다. 내장 디스크는 목록에 나오지 않습니다.',
  step1_empty: '연결된 USB가 없습니다',
  step1_empty_hint: 'USB를 꽂으면 자동으로 나타납니다',
  step1_skipped: '목록에 넣지 못한 장치가 있습니다:',
  refresh: '새로고침',
  refreshing: '찾는 중…',

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
  stage_Analyzing: '원본 분석',
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
  err_write_denied: '장치가 쓰기를 거부했습니다',
  err_write_denied_why:
    '탐색기 창이나 백신이 USB를 붙잡고 있으면 이런 일이 생깁니다. 열려 있는 창을 모두 닫고 USB를 뽑았다 다시 꽂은 뒤 시도해 주세요. 아래에 준비 단계 상태가 함께 표시됩니다.',
  err_media_changed: 'USB가 바뀌었습니다',
  err_media_changed_why: '작업 중에 USB가 분리되었거나 교체되었습니다. 다시 꽂고 처음부터 진행해 주세요.',
  err_identity_changed: '다른 장치입니다',
  err_identity_changed_why: '선택한 USB와 실제 장치가 다릅니다. 무엇이 다른지는 아래에 있습니다.',
  // 준비 단계가 대상의 파티션 테이블을 지운 **뒤에** 실패한 경우.
  // 다른 실패와 반드시 구분해야 한다. "탐색기를 닫고 다시 시도" 같은 안내는
  // USB 가 멀쩡하다는 전제에서만 맞는데, 이 USB 는 이미 비어 있어서 탐색기에
  // 뜨지도 않는다. 그 상태로 그 안내를 보여주면 사용자는 프로그램이 자기 USB 를
  // 망가뜨려 놓고 남 탓을 한다고 읽는다.
  err_target_erased: 'USB를 준비하다 중단됐습니다',
  err_target_erased_why:
    '이 USB의 원래 내용은 이미 지워진 뒤라 되돌릴 수 없습니다. 탐색기에서는 빈 장치로 보이거나 "포맷하시겠습니까"를 물어볼 수 있는데, 그건 고장이 아닙니다. USB를 뽑았다 다시 꽂고 「다시 시도」를 누르면 이어서 끝납니다. 중단된 이유는 아래에 있습니다.',
  // 쓰기는 끝까지 끝났는데 되읽은 내용이 다른 경우. `target_erased` 와 반드시
  // 구분한다 — 저건 "준비하다 멈췄다" 이고 이건 "다 썼는데 대조가 어긋났다" 다.
  // 사용자가 할 일도 다르다. 이 경우 USB 는 완전히 쓰인 상태이므로, 되꽂아
  // 이어서 하라는 안내가 아니라 **그 USB 를 믿지 말라**는 안내가 맞다.
  err_verify_mismatch: '되읽은 내용이 쓴 것과 다릅니다',
  err_verify_mismatch_why:
    '이미지는 끝까지 쓰였지만, 다시 읽어 대조해 보니 일부가 달랐습니다. USB 가 쓰기를 받아들인 척하고 실제로는 저장하지 않는 경우에 이렇게 됩니다. 이 USB 로는 부팅되지 않을 수 있으니 그대로 쓰지 마시고, 다른 USB 나 다른 포트로 다시 구워 보세요. 같은 USB 에서 계속 이러면 그 USB 의 수명이 다한 것입니다.',
  verify_at_head:
    '어긋난 곳은 맨 앞 1MiB — 파티션 테이블이 있는 구간입니다. 이 위치는 USB 불량보다 윈도우가 그 구간을 건드렸을 때 나옵니다. 제보해 주시면 도움이 됩니다.',
  verify_at_offset: '처음 어긋난 위치: {0} 지점 (오프셋 {1})',
  err_network: '내려받지 못했습니다',
  err_network_why: '네트워크 연결을 확인하고 다시 시도해 주세요.',
  err_layout_gpt: 'GPT 로 만들어진 USB 는 아직 복제할 수 없습니다.',
  err_layout_gpt_why:
    '이 프로그램이 만드는 로더 USB 는 MBR 입니다. 다른 방식으로 만들어진 USB 로 보입니다.',
  err_layout_nosig: '이 USB 에서 파티션을 찾지 못했습니다.',
  err_layout_nosig_why: '로더가 써진 USB 가 맞는지 확인하고 다시 골라 주세요.',
  err_same_disk: '원본과 같은 USB 입니다',
  err_same_disk_why: '복사해 넣을 USB 는 원본과 다른 것이어야 합니다.',
  err_generic_why: '문제가 계속되면 아래 내용을 함께 알려주세요.',
  retry: '다시 시도',

  done_title: 'USB가\n준비됐어요',
  done_lead:
    '이 USB로 부팅한 뒤, 로더 화면에서 시놀로지 모델을 선택하시면 됩니다.',
  done_written: '{0} {1} · {2} 기록',
  done_verified: '검증 완료 — 쓴 내용과 USB의 내용이 일치합니다',
  done_explorer_title: '탐색기에서 내용이 안 보이는 것은 정상입니다',
  done_explorer_body:
    '로더 이미지는 리눅스 파티션 구조라 윈도우가 대부분 읽지 못합니다. "포맷하세요" 라고 물어봐도 취소를 누르세요. 제대로 됐는지는 이 USB로 부팅해보면 알 수 있습니다.',
  done_replug: 'USB를 뽑았다 다시 꽂으면 일부 내용이 보일 수 있습니다.',
  eject: '안전하게 제거',
  ejecting: '제거하는 중…',
  eject_ok: '이제 USB를 뽑으셔도 됩니다',
  eject_fail: '제거하지 못했습니다',
  eject_fail_why:
    '무언가 아직 USB를 사용 중입니다. 탐색기 창을 닫고 다시 눌러보세요. 계속 안 되면 작업 표시줄의 "하드웨어 안전하게 제거"를 이용하시면 됩니다.',

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
  reason_same_disk: '원본과 같은 USB 입니다',
};

const en: Dict = {
  simulated: 'Showing simulated devices. These are not real USB drives.',

  app_title: 'Xpenology USB',
  mode_burn_title: 'Write a loader',
  mode_burn_sub: 'Download the latest loader and write it to a USB drive',
  mode_clone_title: 'Clone a USB',
  mode_clone_sub: 'Copy a USB drive you already prepared onto another one',

  clone_pick_source: 'Choose the drive\nto copy from',
  clone_pick_source_hint:
    'The one that already has a loader on it. It is only read, never changed.',
  clone_pick_target: 'Choose the drive\nto copy onto',
  clone_pick_target_hint: 'Everything on this drive will be erased.',
  clone_confirm_title: 'Is this the\nright way round?',
  clone_source: 'Source (read only)',
  clone_target: 'Target (erased)',
  clone_amount: 'To copy',
  clone_partitions: '{0} partitions',
  clone_go: 'Start cloning',
  clone_analyzing: 'Reading the source…',
  clone_done: 'Clone\nfinished',
  clone_done_sub: 'Copied {0} onto {1}',
  verify_label:
    'Read the drive back and compare it. Turning this off is about twice as fast, but you will not know the USB took the image until you boot from it',

  step1_title: 'Choose\nyour USB drive',
  step1_lead:
    'Only USB storage is listed. Internal disks never appear here.',
  step1_empty: 'No USB drive connected',
  step1_empty_hint: 'Plug one in and it will show up automatically',
  step1_skipped: 'Some devices could not be listed:',
  refresh: 'Refresh',
  refreshing: 'Scanning…',

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

  stage_Analyzing: 'Analysing source',
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
  err_write_denied: 'The drive refused the write',
  err_write_denied_why:
    'This usually means something still holds the drive -- an Explorer window or antivirus. Close any windows using it, unplug and replug the drive, then try again. The preparation details are shown below.',
  err_media_changed: 'The drive changed',
  err_media_changed_why:
    'It was unplugged or swapped during the operation. Reconnect it and start over.',
  err_identity_changed: 'This is a different device',
  err_identity_changed_why:
    'The drive no longer matches the one you selected. What differs is shown below.',
  err_target_erased: 'Stopped while preparing the drive',
  err_target_erased_why:
    "This drive's original contents are already gone and cannot be recovered. Explorer may show it as empty or offer to format it -- that is expected, not a fault. Unplug it, plug it back in, and press Try again to finish. The reason it stopped is shown below.",
  err_verify_mismatch: 'The drive read back differently',
  err_verify_mismatch_why:
    'The image was written all the way through, but reading it back found bytes that do not match. That happens when a drive accepts a write and quietly does not store it. This drive may not boot, so do not rely on it -- try burning again on a different drive or a different port. If the same drive keeps doing this, it has worn out.',
  verify_at_head:
    'The mismatch is in the first 1 MiB, where the partition table lives. That location points at something else touching the drive rather than at a faulty stick. Please report it.',
  verify_at_offset: 'First mismatch at {0} (offset {1})',
  err_network: 'Download failed',
  err_network_why: 'Check your network connection and try again.',
  err_layout_gpt: 'This drive uses GPT, which cannot be cloned yet.',
  err_layout_gpt_why:
    'The loader drives this program writes use MBR, so this one was made some other way.',
  err_layout_nosig: 'No partitions found on this drive.',
  err_layout_nosig_why:
    'Make sure you picked the drive that has the loader on it.',
  err_same_disk: 'Same drive as the source',
  err_same_disk_why: 'The drive you copy onto has to be a different one.',
  err_generic_why: 'If this keeps happening, please include the details below.',
  retry: 'Try again',

  done_title: 'Your USB drive\nis ready',
  done_lead:
    'Boot from this drive, then choose your Synology model in the loader screen.',
  done_written: '{0} {1} · {2} written',
  done_verified: 'Verified — what was written matches what is on the drive',
  done_explorer_title: 'Explorer showing nothing is expected',
  done_explorer_body:
    'The loader image uses Linux partitions that Windows mostly cannot read. If it offers to format the drive, choose Cancel. The real test is booting from it.',
  done_replug: 'Unplugging and reconnecting the drive may reveal part of its contents.',
  eject: 'Safely remove',
  ejecting: 'Removing…',
  eject_ok: 'You can unplug the drive now',
  eject_fail: 'Could not remove it',
  eject_fail_why:
    'Something is still using the drive. Close any Explorer windows and press it again. If it keeps failing, use "Safely Remove Hardware" from the taskbar.',

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
  reason_same_disk: 'Same drive as the source',
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
