# ClaimGuard brand (v2: Claims administration)

**ClaimGuard · Claims administration**

The mark is a navy shield that holds a white claim document with a check: every claim is protected and verified before it reaches the payer. The identity is flat navy and white, calm and readable for people who review claims all day. It follows the ClaimGuard administration template (navy sidebar, white cards on a light grey page, a single centred sign-in card).

Open [brand_sheet.html](brand_sheet.html) in a browser for the standalone sheet. Inside the console, every level can open **You → Brand** (`#/brand`).

## Files

| File | Use |
|---|---|
| `claimguard-mark.svg`, `claimguard-mark-512.png` | App icon, favicon, sign-in card (also `src/access/ui/icon.svg`) |
| `claimguard-mark-light.svg` | White mark for navy backgrounds: the sidebar (also `src/access/ui/icon-light.svg`) |
| `claimguard-logo.svg`, `claimguard-logo@3x.png` | Full logo on white or light surfaces (also `src/access/ui/logo.svg`) |
| `claimguard-logo-dark.svg`, `claimguard-logo-dark@3x.png` | Full logo on Administration Navy (also `src/access/ui/logo-dark.svg`) |

The logo SVGs use live text (IBM Plex Sans, falling back to Segoe UI). For print or external design tools, use the PNGs, or convert the text to outlines first.

## Palette

| Name | Hex | Role | On white | On navy |
|---|---|---|---|---|
| Administration Navy | `#15294C` | Sidebar, primary buttons, the shield | 14.4 | - |
| Deep Navy | `#0E1D38` | Shield rim, dark surfaces | 16.8 | - |
| Steel Blue | `#243E70` | Open menu item, hover on navy | 10.5 | - |
| Ink | `#13233F` | Titles and body text | 15.7 | - |
| Slate | `#5B6578` | Labels, table headers, hints | 5.9 | 2.5 |
| Mist | `#9FB0CC` | Secondary text on navy only | 2.2 | 6.6 |
| Line Grey | `#E2E6EC` | Card borders, table rules | - | - |
| Paper | `#F6F7F9` | Page background (cards are white) | - | - |
| Pass Green | `#187A4B` | Passed rule, done | 5.4 | 2.7 |
| Caution Amber | `#9A5B05` | Unable to assess, medium severity | 5.4 | 2.7 |
| Fail Red | `#B42318` | Failed rule, high severity | 6.6 | 2.2 |

Ratios are WCAG contrast ratios, computed with the WCAG formula. White text on navy is 14.4:1, and 10.5:1 on Steel Blue.

Dark mode keeps the same structure: page `#0B1220`, cards `#121B2D`, sidebar `#0E1A33`, accent `#9DB7FF` (8.7:1 on the cards).

## In the console

- **Sidebar:** white mark, "ClaimGuard" and "CLAIMS ADMINISTRATION" in spaced small capitals, then a text-only menu with a Steel Blue pill on the open page. In the narrow menu (rail), the icons come back.
- **Pages:** large navy titles, white cards with thin grey borders, and tables with uppercase letter-spaced headers.
- **Sign-in:** one centred card with the mark, the name, the line and a navy "Sign in" button. In the local demo, the staff cards sit below it.
- **Tab title:** "Page — ClaimGuard Administration".
- **Clearance levels:** each level keeps its own colour (band on top of the page, ring around the avatar, level badge, tint of the sidebar). These colours show the employee's level, not decoration (docs/62, section 8).
- **Customize:** "Brand" (navy) is the default accent; anyone can still pick another accent, a background and a profile picture (docs/62, sections 4, 8 and 9).
- **Tagline:** "Smarter checks, safer claims" stays available for presentations; the product line in the interface is "Claims administration".

Version 1 of the identity (the violet-cyan "AI" shield) is replaced by this one.
