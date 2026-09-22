import type { SVGProps } from 'react'

// Inline rather than lucide-react: six icons are not worth a dependency.
function Icon({ children, ...props }: SVGProps<SVGSVGElement>) {
  return (
    <svg
      viewBox="0 0 24 24"
      width="20"
      height="20"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      {...props}
    >
      {children}
    </svg>
  )
}

export const TerminalIcon = (props: SVGProps<SVGSVGElement>) => (
  <Icon {...props}>
    <rect x="2" y="4" width="20" height="16" rx="2" />
    <path d="m6 9 3 3-3 3M12 15h5" />
  </Icon>
)

export const LazyIcon = (props: SVGProps<SVGSVGElement>) => (
  <Icon {...props}>
    <path d="M4 6h16M4 12h6M4 18h4" />
    <path d="M16 11v8m-3-3 3 3 3-3" />
  </Icon>
)

export const LiveIcon = (props: SVGProps<SVGSVGElement>) => (
  <Icon {...props}>
    <path d="M20 12a8 8 0 0 1-14.9 4M4 12a8 8 0 0 1 14.9-4" />
    <path d="M19 3v5h-5M5 21v-5h5" />
  </Icon>
)

export const DriveIcon = (props: SVGProps<SVGSVGElement>) => (
  <Icon {...props}>
    <rect x="3" y="13" width="18" height="7" rx="2" />
    <path d="M5.5 13 8 5h8l2.5 8" />
    <path d="M7 16.5h.01M11 16.5h.01" />
  </Icon>
)

export const LockIcon = (props: SVGProps<SVGSVGElement>) => (
  <Icon {...props}>
    <rect x="4" y="10" width="16" height="10" rx="2" />
    <path d="M8 10V7a4 4 0 0 1 8 0v3" />
  </Icon>
)

export const BrowserIcon = (props: SVGProps<SVGSVGElement>) => (
  <Icon {...props}>
    <rect x="2" y="4" width="20" height="16" rx="2" />
    <path d="M2 9h20M6 6.5h.01M9 6.5h.01" />
  </Icon>
)

