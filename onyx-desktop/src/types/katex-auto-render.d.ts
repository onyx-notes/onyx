declare module "katex/contrib/auto-render" {
  interface AutoRenderOptions {
    delimiters?: { left: string; right: string; display: boolean }[];
    ignoredTags?: string[];
    throwOnError?: boolean;
  }
  const renderMathInElement: (
    element: HTMLElement,
    options?: AutoRenderOptions,
  ) => void;
  export default renderMathInElement;
}

declare module "katex/dist/katex.min.css";
