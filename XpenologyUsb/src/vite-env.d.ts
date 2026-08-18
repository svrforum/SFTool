/// <reference types="vite/client" />

// Vite 가 제공하는 앰비언트 타입을 끌어온다. `import './styles.css'` 같은
// 부수효과 임포트와 `import.meta.env` 가 여기서 선언된다.
//
// TypeScript 5 는 선언이 없는 CSS 임포트를 그냥 넘겼지만 7 부터는 TS2882 로
// 막는다. 원래 Vite 스캐폴드에 들어 있는 파일인데 빠져 있었다.
