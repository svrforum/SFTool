import { expect, test, type Page, type TestInfo } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

async function capture(page: Page, info: TestInfo, name: string) {
  const path = info.outputPath(`${name}.png`);
  await page.screenshot({ path, animations: "disabled" });
  await info.attach(name, { path, contentType: "image/png" });
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  expect(
    await page
      .locator("main")
      .evaluate((el) => el.scrollWidth <= el.clientWidth + 1),
  ).toBe(true);
  for (const button of await page.locator("footer button").all()) {
    const box = await button.boundingBox();
    expect(box).not.toBeNull();
    expect(box!.y).toBeGreaterThanOrEqual(0);
    expect(box!.y + box!.height).toBeLessThanOrEqual(
      page.viewportSize()!.height,
    );
  }
}

async function accessible(page: Page) {
  const result = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21aa"])
    .analyze();
  expect(result.violations).toEqual([]);
}

test("burn and clone screens: keyboard, consent, completion, themes, and layout", async ({
  page,
}, info) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const [lang, theme] = info.project.name.split("-");
  await page.addInitScript(
    ({ lang, theme }) => {
      localStorage.setItem("sftool-language", lang);
      localStorage.setItem("sftool-theme", theme);
    },
    { lang, theme },
  );
  await page.clock.install();
  await page.goto("/");
  await expect(page.locator("html")).toHaveAttribute("lang", lang);
  await capture(page, info, "01-home");
  await accessible(page);

  await page.locator('[data-mode="burn"]').focus();
  await page.keyboard.press("Enter");
  await expect(page.locator("h1")).toBeFocused();
  await expect(page.locator('[data-go="2"]')).toBeDisabled();
  await page.locator('[data-disk="2"]').click();
  await capture(page, info, "02-device");
  await page.locator('[data-go="2"]').click();
  await capture(page, info, "03-loader");
  await page.locator('[data-go="3"]').click();
  await expect(page.locator('[data-go="4"]')).toBeDisabled();
  await capture(page, info, "04-confirm");
  await accessible(page);
  await page.locator("[data-ack]").focus();
  await page.keyboard.press("Space");
  await expect(page.locator('[data-go="4"]')).toBeEnabled();
  await page.locator('[data-go="4"]').click();
  await capture(page, info, "05-progress");
  await page.clock.runFor(60000);
  await expect(page.locator("[data-eject]")).toBeVisible();
  await expect(page.locator(".steps [aria-current]")).toHaveCount(0);
  await capture(page, info, "06-complete");

  await page.locator('[data-mode="home"]').click();
  await page.locator('[data-mode="clone"]').click();
  await page.locator('[data-disk="2"]').click();
  await expect(page.locator(".selection-strip")).toBeVisible();
  await expect(page.locator('[data-disk="2"]')).toHaveCount(0);
  await page.locator('[data-disk="3"]').click();
  await capture(page, info, "07-clone-confirm");
  await accessible(page);
  await page.locator("[data-ack]").check();
  await page.locator('[data-go="4"]').click();
  await page.locator("[data-cancel]").click();
  await page.clock.runFor(1000);
  await expect(page.locator('[data-mode="clone"]')).toBeVisible();
  await capture(page, info, "08-cancel");
  expect(errors).toEqual([]);
});
