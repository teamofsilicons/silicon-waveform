import { expect,test,type BrowserContext,type Page } from "@playwright/test";
import { accountsSession,runAccounts,signIn,watchErrors } from "./support";
import { mkdirSync } from "node:fs";
test.describe.configure({mode:"serial"});
let context:BrowserContext,page:Page,errors:string[],fileUrl="";
test.beforeAll(async({browser})=>{context=await accountsSession(browser,"carbon");page=await context.newPage();errors=watchErrors(page);await signIn(page,runAccounts().carbon.email,"/speech");mkdirSync("../.mig/screens",{recursive:true});});
test.afterAll(async()=>{await context?.close();});
test("creates speech with the actual API and reuses its Briefcase recording for transcription",async()=>{
 await page.getByRole("textbox",{name:"What should the voice say?"}).fill("A little clarity goes a long way. This recording exercises Waveform through its real Accounts sign-in.");
 await page.getByRole("button",{name:"Create speech",exact:true}).click();
 await expect(page.getByRole("heading",{name:"Your recording"})).toBeVisible({timeout:30000});
 fileUrl=(await page.getByRole("link",{name:"Open recording in Briefcase"}).getAttribute("href"))!;
 expect(fileUrl).toContain("4162");await expect(page.locator("audio")).toBeVisible();
 await page.evaluate(()=>{(document.activeElement as HTMLElement)?.blur();window.scrollTo(0,0);});await page.waitForTimeout(200);await page.screenshot({path:"../.mig/screens/waveform-speech-desktop.png",fullPage:true,caret:"initial"});
 await page.getByRole("button",{name:"Speech to text",exact:true}).click();
 await page.getByRole("textbox",{name:"Briefcase audio URL"}).fill(fileUrl);
 await page.getByRole("button",{name:"Transcribe audio",exact:true}).click();
 await expect(page.getByRole("heading",{name:"Your transcript"})).toBeVisible({timeout:30000});
 await expect(page.getByRole("button",{name:"Copy transcript",exact:true})).toBeVisible();
});
test("history lists both requests and opens the retained first-line details",async()=>{
 await page.goto("/history");
 await expect(page.getByText("Text to speech ·",{exact:false}).first()).toBeVisible();
 await expect(page.getByText("Speech to text ·",{exact:false}).first()).toBeVisible();
 await page.locator("li a").filter({hasText:"A little clarity"}).click();
 await expect(page.getByRole("heading",{name:"Speech details"})).toBeVisible();
 await expect(page.getByText("A little clarity",{exact:false}).first()).toBeVisible();
 await page.evaluate(()=>{(document.activeElement as HTMLElement)?.blur();window.scrollTo(0,0);});await page.waitForTimeout(200);await page.screenshot({path:"../.mig/screens/waveform-history-detail.png",fullPage:true,caret:"initial"});
});
test("provider keys, preferences and Briefcase authorization persist through the API",async()=>{
 await page.goto("/settings");
 const key=page.getByRole("textbox",{name:"OpenAI API key",exact:true});await key.fill("sk-browser-fixture");
 await key.locator("xpath=ancestor::form").getByRole("button",{name:"Save key",exact:true}).click();
 await expect(page.getByText("Key saved.",{exact:true})).toBeVisible();
 await page.reload();await expect(page.getByRole("button",{name:"Hold to remove OpenAI key",exact:true})).toBeVisible();
 const remove=page.getByRole("button",{name:"Hold to remove OpenAI key",exact:true});await remove.focus();await page.keyboard.down("Space");await page.waitForTimeout(1800);await page.keyboard.up("Space");await expect(page.getByText("Key removed.",{exact:true})).toBeVisible();
 const telemetry=page.getByRole("switch",{name:"Share operational telemetry"});await telemetry.click();await expect(page.getByText("Telemetry disabled.",{exact:true})).toBeVisible();await page.reload();await expect(telemetry).not.toBeChecked();
 const revoke=page.getByRole("button",{name:"Hold to revoke access",exact:true});await revoke.focus();await page.keyboard.down("Space");await page.waitForTimeout(1800);await page.keyboard.up("Space");await expect(page.getByText("Briefcase access revoked.",{exact:true})).toBeVisible();
 await page.getByRole("button",{name:"Approve access",exact:true}).click();await expect(page.getByText("Briefcase access approved.",{exact:true})).toBeVisible();
 await page.evaluate(()=>{(document.activeElement as HTMLElement)?.blur();window.scrollTo(0,0);});await page.waitForTimeout(200);await page.screenshot({path:"../.mig/screens/waveform-settings-desktop.png",fullPage:true,caret:"initial"});
});
test("workspace fits a phone in dark mode without client errors",async()=>{
 await page.setViewportSize({width:390,height:844});await page.emulateMedia({colorScheme:"dark",reducedMotion:"reduce"});await page.goto("/speech");
 await expect(page.getByRole("heading",{level:1})).toBeVisible();
 expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+2)).toBe(true);
 await page.evaluate(()=>{(document.activeElement as HTMLElement)?.blur();window.scrollTo(0,0);});await page.waitForTimeout(200);await page.screenshot({path:"../.mig/screens/waveform-speech-phone-dark.png",fullPage:true,caret:"initial"});
 expect(errors).toEqual([]);
});
