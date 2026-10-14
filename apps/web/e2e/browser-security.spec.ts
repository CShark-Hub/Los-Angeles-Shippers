import { expect, test } from '@playwright/test';
import {
	answerText,
	answerableQuestion,
	ask,
	revokeSpareToken,
	SESSION_COOKIE,
	signedInTest,
	signIn,
	spareToken,
	token,
	type Session
} from './support';

/** A page on another site, served by the test: the browser sees it as a different origin. */
const OTHER_SITE = 'http://other.example.test';

/** The session cookie as a request sends it back. */
function cookieOf(session: Session): string {
	const cookie = session.cookies.find((c) => c.name === SESSION_COOKIE);
	expect(cookie).toBeDefined();
	return `${cookie!.name}=${cookie!.value}`;
}

test.describe('What the browser enforces on the web client', () => {
	test.describe('Given the client served through Caddy', () => {
		test.describe('When a page is requested, known or not', () => {
			for (const path of ['/', '/ask/q_unknown', '/signin']) {
				test(`Then ${path} comes with the policy that keeps everything on its own origin`, async ({
					request
				}) => {
					const page = await request.get(path);
					expect(page.status()).toBe(200);
					expect(page.headers()['content-type']).toContain('text/html');
					const policy = page.headers()['content-security-policy'] ?? '';
					expect(policy).toContain("default-src 'self'");
					expect(policy).toContain("connect-src 'self'");
					expect(policy).toContain("frame-ancestors 'none'");
					expect(policy).toMatch(/script-src 'self' 'sha256-[A-Za-z0-9+/=]+'/);
					expect(policy).not.toContain("script-src 'self' 'unsafe-inline'");
					expect(page.headers()['x-content-type-options']).toBe('nosniff');
					expect(page.headers()['referrer-policy']).toBe('no-referrer');
					expect(page.headers()['server']).toBeUndefined();
				});
			}
		});
	});

	signedInTest.describe('Given a signed-in person who has been answered', () => {
		signedInTest.beforeEach(async ({ page }) => {
			await page.goto('/ask');
			await ask(page, answerableQuestion());
			await expect(page.getByRole('region', { name: 'Answer' })).toContainText(answerText());
		});

		signedInTest.describe('When the session cookie is inspected', () => {
			signedInTest(
				'Then JavaScript cannot read it, and it is secure and strictly same-site',
				async ({ page, context }) => {
					const session = (await context.cookies()).find((c) => c.name === SESSION_COOKIE);
					expect(session).toMatchObject({
						httpOnly: true,
						secure: true,
						sameSite: 'Strict',
						path: '/'
					});
					const readable = await page.evaluate(() => document.cookie);
					expect(readable.includes(SESSION_COOKIE)).toBe(false);
				}
			);
		});

		signedInTest.describe('When the browser storage is read', () => {
			signedInTest('Then it holds no token, no answer and no fact', async ({ page }) => {
				const stored = await page.evaluate(async () => ({
					local: JSON.stringify({ ...localStorage }),
					session: JSON.stringify({ ...sessionStorage }),
					databases: JSON.stringify(await indexedDB.databases()),
					caches: JSON.stringify(await caches.keys())
				}));
				const everything = Object.values(stored).join('\n');
				expect(everything.includes(token())).toBe(false);
				expect(everything.includes(answerText().slice(0, 30))).toBe(false);
				expect(/\bf_[A-Za-z0-9]{4,16}\b/.test(everything)).toBe(false);
				expect(stored.databases).toBe('[]');
			});
		});

		signedInTest.describe('When the page asks another site for something', () => {
			signedInTest('Then the policy blocks the request before it leaves', async ({ page }) => {
				const violated = await page.evaluate(
					() =>
						new Promise<string>((done) => {
							document.addEventListener(
								'securitypolicyviolation',
								(e) => done(e.effectiveDirective),
								{ once: true }
							);
							fetch('https://example.com').catch(() => {});
						})
				);
				expect(violated).toBe('connect-src');
			});
		});

		signedInTest.describe('When a script is slipped into the page', () => {
			signedInTest('Then it does not run', async ({ page }) => {
				const ran = await page.evaluate(() => {
					const script = document.createElement('script');
					script.textContent = 'window.__injected = true';
					document.head.append(script);
					return (window as unknown as { __injected?: boolean }).__injected === true;
				});
				expect(ran).toBe(false);
			});
		});
	});

	signedInTest.describe('Given a signed-in person loading the client', () => {
		signedInTest.describe('When they open the Ask page and ask', () => {
			signedInTest(
				'Then every request the client made went to its own origin',
				async ({ page, baseURL }) => {
					const seen: string[] = [];
					page.on('request', (r) => seen.push(r.url()));
					await page.goto('/ask');
					await ask(page, answerableQuestion());
					await expect(page.getByRole('region', { name: 'Answer' })).toBeVisible();
					const origins = new Set(
						seen.filter((url) => /^https?:/.test(url)).map((url) => new URL(url).origin)
					);
					expect([...origins]).toEqual([new URL(baseURL!).origin]);
				}
			);
		});
	});

	test.describe('Given a page on another site', () => {
		test.beforeEach(async ({ page, baseURL }) => {
			await page.route(`${OTHER_SITE}/**`, (route) => {
				const framed = new URL(route.request().url()).pathname === '/frame';
				const body = framed
					? `<iframe src="${baseURL}/signin" title="the app"></iframe>`
					: `<form method="post" action="${baseURL}/v1/questions">
					     <input name="text" value="anything" />
					     <button type="submit">go</button>
					   </form>`;
				return route.fulfill({ contentType: 'text/html', body });
			});
		});

		test.describe('When it embeds the client in a frame', () => {
			test.beforeEach(async ({ page }) => {
				await page.goto(`${OTHER_SITE}/frame`);
				await page.waitForLoadState('load');
			});

			test('Then the browser refuses to show it', async ({ page, browserName }) => {
				const framed = page.frames().filter((f) => f !== page.mainFrame());
				expect(framed).toHaveLength(1);
				if (browserName === 'firefox') {
					// Firefox leaves a refused frame empty: no address, and no document to read
					await expect.poll(() => framed[0].url()).toBe('');
					return;
				}
				await expect(framed[0].getByRole('heading', { name: 'Sign in to Mercury' })).toBeHidden();
				if (browserName === 'webkit') {
					// WebKit keeps the address and leaves the document empty
					expect(await framed[0].evaluate(() => document.body?.innerText ?? '')).toBe('');
				} else {
					// Chromium and Edge put their error page in the frame, at an address of their own
					await expect.poll(() => framed[0].url()).not.toContain('/signin');
				}
			});
		});

		test.describe('When it posts a form to the question route', () => {
			test.beforeEach(async ({ page }) => {
				await page.goto(`${OTHER_SITE}/form`);
			});

			test('Then the browser sends no session with it, and the core refuses', async ({ page }) => {
				const answered = page.waitForResponse(
					(r) => r.url().endsWith('/v1/questions') && r.request().method() === 'POST'
				);
				await page.getByRole('button', { name: 'go' }).click();
				const response = await answered;
				expect(await response.request().headerValue('cookie')).toBeNull();
				expect(response.status()).toBe(401);
			});
		});
	});

	signedInTest.describe(
		'Given a session and a request that names another site as its origin',
		() => {
			signedInTest.describe('When it posts a question with the session cookie', () => {
				signedInTest('Then the core answers 403', async ({ request, session }) => {
					const forged = await request.post('/v1/questions', {
						headers: {
							cookie: cookieOf(session!),
							origin: OTHER_SITE,
							'x-mercury-client': 'web',
							'idempotency-key': 'e2e-cross-site'
						},
						data: { text: answerableQuestion(), project: 'delta' }
					});
					expect(forged.status()).toBe(403);
				});
			});
		}
	);
	signedInTest.describe('Given a signed-in person whose page asks the door for agents', () => {
		signedInTest.describe('When the page sends its session cookie to /mcp', () => {
			signedInTest(
				'Then it is refused, tells nothing of the tools, and the session stands',
				async ({ page }) => {
					await page.goto('/ask');
					const answered = await page.evaluate(async () => {
						const response = await fetch('/mcp', {
							method: 'POST',
							credentials: 'include',
							headers: {
								'content-type': 'application/json',
								accept: 'application/json, text/event-stream'
							},
							body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/list', params: {} })
						});
						return { status: response.status, body: await response.text() };
					});
					expect(answered.status).toBe(401);
					expect(answered.body).not.toContain('"tools"');
					// the page itself still has its session: its own request, with its own cookie
					const me = await page.evaluate(() => fetch('/v1/me').then((r) => r.status));
					expect(me).toBe(200);
				}
			);
		});
	});

	test.describe('Given a person signed in on a token that is then revoked', () => {
		test.describe('When their open page makes its next request', () => {
			test('Then the session is over, they are at Sign in, and the cookie opens nothing', async ({
				page,
				context,
				request
			}) => {
				await signIn(page, spareToken());
				await expect(page).toHaveURL(/\/ask$/);
				const session = (await context.cookies()).find((c) => c.name === SESSION_COOKIE);
				expect(session).toBeDefined();
				const cookie = `${session!.name}=${session!.value}`;
				expect((await request.get('/v1/me', { headers: { cookie } })).status()).toBe(200);

				revokeSpareToken();

				// the next request the page makes: asking
				await ask(page, answerableQuestion());
				await expect(page).toHaveURL(/\/signin$/);
				await expect(page.getByRole('heading', { name: 'Sign in to Mercury' })).toBeVisible();
				expect((await request.get('/v1/me', { headers: { cookie } })).status()).toBe(401);
			});
		});
	});

	test.describe('Given a visitor who keeps trying tokens', () => {
		test.describe('When they pass the sign-in limit', () => {
			test('Then the page tells them to wait, in plain words, and keeps nothing they typed', async ({
				page
			}) => {
				await page.goto('/signin');
				const field = page.getByLabel('Your token');
				const limited = page.getByRole('alert').filter({ hasText: 'Too many sign-in attempts' });
				for (let attempt = 1; attempt <= 12 && !(await limited.isVisible()); attempt += 1) {
					await field.fill(`mcy_not_a_token_${attempt}`);
					await page.getByRole('button', { name: 'Sign in' }).click();
					await expect(page.getByRole('alert')).toBeVisible();
				}
				await expect(limited).toBeVisible();
				await expect(limited).toContainText(/Wait \d+ seconds/);
				await expect(field).toHaveValue('');
				await expect(page).toHaveURL(/\/signin$/);
			});
		});
	});
});
