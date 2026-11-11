import { WORDMARK_VIEWBOX, WORDMARK_WORD } from "@/components/wordmark-paths";

/**
 * The vigily mark: a pair of binoculars, keeping watch. The same geometry as binoculars() in
 * assets/src/build.py, cropped to the drawing, coloured from the --logo-* tokens of each theme.
 */
export function Mark({ className }: { className?: string }) {
  return (
    <svg viewBox="5.5 18.75 53 26.25" className={className} aria-hidden="true" focusable="false">
      <rect x="18.5" y="28.25" width="27" height="7.5" rx="3.75" fill="var(--logo-body)" />
      <rect x="28" y="23.25" width="8" height="17.5" rx="3.75" fill="var(--logo-body)" />
      <rect x="25.125" y="19.25" width="13.75" height="7.5" rx="2.625" fill="var(--logo-body)" />
      {[18.5, 45.5].map((cx) => (
        <g key={cx}>
          <circle cx={cx} cy="32" r="12.5" fill="var(--logo-body)" />
          <circle cx={cx} cy="32" r="7.5" fill="var(--logo-glass)" />
          <path d={`M${cx - 4.59} 30.77A4.75 4.75 0 0 1 ${cx - 0.41} 27.27`} fill="none" stroke="var(--logo-glint)" strokeWidth="1.62" strokeLinecap="round" />
        </g>
      ))}
    </svg>
  );
}

/** The wordmark letters, plain. Decorative: place it next to the text "vigily" for assistive technology. */
export function WordmarkText({ className }: { className?: string }) {
  return (
    <svg viewBox={WORDMARK_VIEWBOX} className={className} aria-hidden="true" focusable="false">
      <path d={WORDMARK_WORD} fill="currentColor" />
    </svg>
  );
}
