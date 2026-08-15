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
