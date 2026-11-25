import ReactMarkdown from "react-markdown";

const SAFE_HREF = /^(https?:|mailto:)/i;

/** Inert Markdown: no raw HTML (react-markdown without rehype-raw escapes it to text). */
export function Markdown({ text }: { text: string }) {
  return (
    <ReactMarkdown
      urlTransform={(url) => (SAFE_HREF.test(url) ? url : "")}
      components={{
        a({ href, children }) {
          if (!href || !SAFE_HREF.test(href)) return <span>{children}</span>;
          return (
            <a href={href} rel="noreferrer noopener" target="_blank" className="underline">
              {children}
            </a>
          );
        },
      }}
    >
      {text}
    </ReactMarkdown>
  );
}
