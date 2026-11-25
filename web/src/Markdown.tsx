import ReactMarkdown from "react-markdown";

const SAFE_HREF = /^https?:\/\//i;

/** Only http(s) links are allowed; everything else is rendered as plain text. */
export const isSafeHref = (href: string | null | undefined): href is string => !!href && SAFE_HREF.test(href);

/** Inert Markdown: no raw HTML (no rehype-raw), no images (alt text only), http(s) links only. */
export function Markdown({ text }: { text: string }) {
  return (
    <ReactMarkdown
      urlTransform={(url) => (isSafeHref(url) ? url : "")}
      components={{
        img: ({ alt }) => <span>{alt}</span>,
        a({ href, children }) {
          if (!isSafeHref(href)) return <span>{children}</span>;
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
