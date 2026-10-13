/**
 * The question on screen is named in the address, `/ask/<question id>`, because an id is all a
 * reload needs to find it again, and nothing about the question is kept in the browser.
 */

/** The question an address names: the part after `/ask/`, or none. */
export function questionIn(pathname: string): string | null {
	const [, route, id] = pathname.split('/');
	if (route !== 'ask' || !id) return null;
	try {
		return decodeURIComponent(id);
	} catch {
		// an address that isn't valid encoding names no question
		return null;
	}
}
