# Windows Defender 오탐 대응

`XpenologyUsb-portable.exe` 가 `Behavior:Win32/Persistence.A!ml` 로 잡히고, 내려받는
즉시 삭제되는 상태다. 이 문서는 그때 무엇을 해야 하는지 적어 둔 것이다.

## 왜 잡히는가

이 프로그램이 하는 일은 백신 입장에서 부트킷과 구분되지 않는다.

- 서명되지 않은 실행 파일이
- 관리자 권한을 요구하고
- `\\.\PhysicalDriveN` 을 열어 원시 섹터를 쓰고
- **디스크의 첫 섹터(파티션 테이블)를 덮어쓴다**

마지막 항목이 MITRE ATT&CK 의 T1542.003(Bootkit)과 같은 동작이다. 부트로더를 굽는
일 자체가 그것이므로 코드로 없앨 수 있는 부분이 아니다.

내려받자마자 삭제되는 것은 여기서 한 단계 더 간 상태다. 실행해 보고 판단하는 것이
아니라 **파일 자체가 차단**된 것이고, 클라우드 평판에 올라갔다는 뜻이다. 새로 빌드해
해시가 바뀌어도 같은 판정을 받을 수 있다.

## 무엇이 방아쇠인지 — 아직 모른다

버전별로는 0.4.2 가 정상이고 0.4.3 부터 잡혔다. 0.4.3 은 Win32 호출을 하나도
추가하지 않았고 순서만 바꿨다 — 되읽기 검증을 파티션 테이블보다 **앞으로**
옮겼다. 그래서 "디스크를 전부 훑은 뒤 부트섹터를 찍는" 순서가 됐다.

**다만 이것을 원인으로 단정하면 안 된다.** 확인된 반례와 한계가 있다.

- 오프셋 0 을 마지막에 쓰는 것 자체는 **0.2.1 부터** 있었다 (`f46e0c8`).
  0.4.3 이 새로 만든 것은 그 앞에 놓인 전체 되읽기뿐이다.
- `Behavior:*!ml` 의 `Persistence` 는 **일반적인 ML 분류 이름**이다. 그 단어에서
  구체적인 동작을 역추적하는 것은 근거가 되지 않는다.
- 이 저장소의 릴리스별 내려받기 수는 **2~9 회**다. 평판이 사실상 0 인 서명 없는
  실행 파일은 같은 코드라도 빌드마다 판정이 갈릴 수 있다. 버전 간 비교가
  코드 차이를 보는 것인지 운을 보는 것인지 구분되지 않는다.

### 확정하는 방법

탐지가 난 기계에서 PowerShell(관리자):

```powershell
Get-MpThreatDetection | Format-List *
Get-MpThreat
```

`DetectionSourceTypeID` 가 답이다 — **4** 면 내려받기 검사, **3** 이면 실시간
보호, **7** 이면 동작 감시다. `ProcessName` 은 브라우저인지 이 프로그램인지
알려준다. 이걸 읽기 전에 코드를 또 고치는 것은 추측이다. 그 추측으로 이미 두
번 틀렸다.

더 깊이 보려면 관리자 권한으로 `MpCmdRun.exe -GetFiles` 를 돌린 뒤
`C:\ProgramData\Microsoft\Windows Defender\Support\MpSupportFiles.cab` 안의
MPLog 를 본다. 동작 감시가 무엇을 보고 판정했는지 그 안에 있다.

## 지금 할 일 (순서대로)

### 1. Microsoft 에 오탐 신고

**이미 내려진 클라우드 판정을 지우는 유일한 방법이다.** 내려받자마자 삭제되는
증상이 그 판정 때문이므로, 코드를 고쳐도 이것부터 풀지 않으면 배포가 막힌다.

https://www.microsoft.com/en-us/wdsi/filesubmission

- **Submission type**: Software developer
- **Detection name**: `Behavior:Win32/Persistence.A!ml`
- 지금까지 배포한 버전을 **전부** 올린다. 판정은 해시 하나가 아니라 비슷한
  파일 무리에 걸린다.
- 위의 `MpSupportFiles.cab` 도 함께 올리면 판단 근거가 된다.
- 로그인해서 넣어야 결과를 읽을 수 있다.

**처리 기간은 공개된 기준이 없다.** 며칠에서 몇 주까지 걸리고, 영향 범위가 큰
쪽이 먼저 처리된다. 그리고 **앞으로 빌드할 파일까지 덮어주지 않는다** — 새
릴리스는 다시 걸릴 수 있다.

### 2. 릴리스 간격을 늘린다

이것이 이 목록에서 가장 저평가된 항목이다. 릴리스마다 새 해시가 생기고, 새
해시는 평판 0 에서 시작한다. 사흘에 일곱 개를 내면 **어느 것도 평판을 쌓지
못한다.** 고칠 것을 모아서 주 단위로 내고, `XpenologyUsb-portable.exe` 라는
파일 이름은 영원히 바꾸지 않는다.

### 3. 코드 서명 — 무료 경로부터

근본 해결책이다. 순서대로 시도할 것.

**SignPath Foundation** (https://signpath.org/apply) — 오픈소스 프로젝트에
**무료로** 서명해 준다. HSM 기반이고 GitHub Actions 안에서 서명이 돌아간다.
공개 저장소이고 MIT 라이선스이므로 자격 요건은 맞는다. 릴리스마다 같은 신원이
붙어 평판이 누적되는 유일한 무료 경로다. 신청할 때 "Rufus, balenaEtcher 와 같은
부류의 USB 이미지 기록 도구" 로 설명할 것 — 심사 기준에 맬웨어·PUA 항목이 있다.

**Certum Open Source Code Signing** — 개인 명의로 받을 수 있고 연 $50 안팎으로
알려져 있다 (shop.certum.eu 에서 직접 확인할 것). SimplySign 클라우드로 쓰므로
하드웨어 토큰을 통관시킬 필요가 없다. 인증서에 본인 이름이 들어가서 장기적으로는
SignPath 보다 나은 신원이 된다.

**Azure Trusted Signing 은 지금은 안 된다.** 개인 개발자 신원 확인이 미국과
캐나다에만 열려 있어서 한국 개인은 가입할 수 없다. 법인이 있으면 다른 이야기다.

**EV 인증서는 사지 말 것.** 한국 리셀러 기준 78~95만원인데, Microsoft 문서가
EV 로 SmartScreen 평판을 사는 효과는 **더 이상 없다**고 명시한다. Rufus 는
EV 서명이 붙어 있는데도 여전히 오탐을 겪는다.

### 4. 사용자에게 알리기

배포처(포럼 글, 릴리스 노트)에 이 경고가 뜬다는 사실과 이유를 미리 적어 둔다.
아무 설명 없이 백신이 지우면 사용자는 프로그램이 실제로 악성이라고 판단한다.

## 코드로 할 수 있는 것과 없는 것

**없는 것.** 원시 디스크 쓰기와 첫 섹터 덮어쓰기는 이 프로그램의 기능 그 자체다.

**있는 것.** 점수를 더하는 군더더기를 만들지 않는 것. 다만 이미 있는 동작을
빼는 것은 **하지 말 것** — 잠금 순서, `FSCTL_ALLOW_EXTENDED_DASD_IO`, 꼬리
지우기는 전부 실물에서 재현된 버그를 고치느라 생긴 것이고, 탐지에 기여한다는
근거는 없다. 추측으로 빼면 고쳐 둔 버그가 돌아온다.

- 0.4.4 에서 탐색기 창을 막으려고 `IOCTL_MOUNTMGR_SET_AUTO_MOUNT` 를 불렀다.
  이것은 `HKLM\SYSTEM\CurrentControlSet\Services\mountmgr\NoAutoMount` 에 쓰는,
  **프로세스가 끝난 뒤에도 남는 시스템 전역 변경**이다. 0.4.5 에서 되돌렸다.
- 실행 파일의 게시자·저작권·설명을 채워 둔다 (`tauri.conf.json` 의 `bundle`).
  비어 있으면 만든 사람을 알 수 없는 프로그램으로 보인다.

**새 Win32 호출을 넣을 때마다** "이게 프로세스 밖에 무언가를 남기는가" 를 먼저
묻는다. 남긴다면 그 기능은 이 프로그램에 넣지 않는 편이 낫다.

## 신고 문구

```
XpenologyUsb is an open-source utility that writes Xpenology bootloader
images (m-shell / RR) to a USB drive, in the same way as Rufus in DD mode
or Win32 Disk Imager.

It is detected as Behavior:Win32/Persistence.A!ml. We believe this is a
false positive caused by the nature of the tool rather than by anything
malicious:

- It opens \\.\PhysicalDriveN and writes raw sectors, because writing a
  bootable USB drive requires exactly that.
- It writes the image's MBR to sector 0, because a bootable drive needs a
  partition table. The application deliberately writes this last so that
  Windows cannot mount the volume mid-write.
- It requests administrator rights, because raw disk access requires them.
- It is not code signed. We are an individual developer.

The application does not persist anything on the system. It creates no
registry entries, services, scheduled tasks or startup items, and writes
no files outside the USB drive the user selected. The only thing that
survives the process exiting is the bootloader image on that drive.

Full source: https://github.com/svrforum/SFTool
The binaries are built from that source by GitHub Actions, and every
release publishes its SHA-256 hashes together with a link to the build
log, so any submitted file can be traced back to the exact commit it came
from.

Affected files (SHA-256):
  7afae2b777fa2da392ee0012a88e57480d2d231f66b31b5f0bbe7b5d78c2fd34  0.4.0
  4bf4acc7660c252a278fe7713d88ea900295473df33dfa83392afbb6871ff631  0.4.1
  efb1ff1fc96298854b90bbfd52e5a80ebed878123c6fe6756128cccd84b10b35  0.4.2
  f75fe4091b315998eb82d47ad76c98b339a68252873c788eb7d4f9ccb0e9b2ea  0.4.3
  57e14f659e942c990081682a84451048617d23c659d305155363e5a617deb242  0.4.4
  ad4a702b2bbd4391fa43eda1a96f6fc97b03480cfcdc5553904b39ab62799550  0.4.6
```

## 사용자용 안내 문구

릴리스 노트와 배포 글에 넣을 것:

```
백신이 이 파일을 지울 수 있습니다

USB에 부트로더를 직접 굽는 프로그램이라, 백신 입장에서는 디스크의 첫 섹터를
건드리는 동작이 부트킷과 구분되지 않습니다. 여기에 코드 서명이 없어서
Windows Defender 가 차단하는 경우가 있습니다. Rufus 같은 도구도 서명을 받기
전에는 같은 문제를 겪었습니다.

Microsoft 에 오탐 신고를 해 둔 상태입니다.

이 프로그램은 시스템에 아무것도 남기지 않습니다. 레지스트리·서비스·시작
프로그램을 만들지 않고, 선택하신 USB 밖으로는 파일을 쓰지 않습니다.

소스는 전부 공개돼 있고 GitHub Actions 가 그 소스로 빌드합니다. 릴리스마다
SHA-256 해시와 빌드 기록 링크를 함께 올리니, 받으신 파일이 정말 그 소스에서
나온 것인지 직접 확인하실 수 있습니다.
```
