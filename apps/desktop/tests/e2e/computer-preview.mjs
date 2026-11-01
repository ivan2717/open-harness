import assert from 'node:assert/strict'
import { mkdtemp, mkdir, rm } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { _electron, expect } from '@playwright/test'
const require = createRequire(import.meta.url)
const data = await mkdtemp(join(tmpdir(), 'openharness-preview-e2e-'))
const env = { ...process.env, OPENHARNESS_DESKTOP_DATA_DIR: data }
delete env.ELECTRON_RUN_AS_NODE
let application
const errors = []
try {
  await mkdir(resolve('test-results'), { recursive: true })
  application = await _electron.launch({
    executablePath: require('electron'),
    args: [resolve('.')],
    env,
  })
  const page = await application.firstWindow()
  page.on('pageerror', (error) => errors.push(error.message))
  await expect(page.getByRole('heading', { name: '开始一个新任务' })).toBeVisible()
  const jpeg = (await page.screenshot({ type: 'jpeg', quality: 40 })).toString('base64')
  const emit = (value) =>
    application.evaluate(
      ({ BrowserWindow }, value) =>
        BrowserWindow.getAllWindows()[0].webContents.send('openharness:computer:preview', value),
      value,
    )
  const source = { id: 'fixture', active: true, title: '测试应用', width: 1200, height: 800, jpeg }
  await emit(source)
  const panel = page.getByRole('region', { name: '电脑操作预览' })
  const surface = panel.getByLabel('拖动预览位置，也可使用方向键移动')
  await expect(panel).toBeVisible()
  await expect(panel.getByRole('img', { name: /的实时操作画面/ })).toBeVisible()
  // Check pixel refresh, not just a changing IPC payload or image attribute.
  const frame = panel.getByRole('img', { name: /的实时操作画面/ })
  const beforeFrame = await frame.screenshot()
  const nextJpeg = await page.evaluate(() => {
    const canvas = document.createElement('canvas')
    canvas.width = 1200
    canvas.height = 800
    const context = canvas.getContext('2d')
    context.fillStyle = '#406552'
    context.fillRect(0, 0, canvas.width, canvas.height)
    return canvas.toDataURL('image/jpeg').split(',')[1]
  })
  await emit({ ...source, jpeg: nextJpeg })
  await expect(frame).toHaveAttribute('src', `data:image/jpeg;base64,${nextJpeg}`)
  assert.ok(!(await frame.screenshot()).equals(beforeFrame), 'Incoming frames repaint the image')
  // A preview received while minimized must repaint when the app is restored.
  await application.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].minimize())
  await emit(source)
  await application.evaluate(({ BrowserWindow }) => {
    const window = BrowserWindow.getAllWindows()[0]
    window.restore()
    window.show()
  })
  await expect(frame).toHaveAttribute('src', `data:image/jpeg;base64,${jpeg}`)
  assert.ok(
    (await frame.screenshot()).equals(beforeFrame),
    'Restoring the window shows the latest frame',
  )
  assert.equal(
    await application.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows().length),
    1,
    'No separate preview BrowserWindow',
  )
  const start = await surface.boundingBox()
  await page.mouse.move(start.x + 75, start.y + 20)
  await page.mouse.down()
  await page.mouse.move(30, 80, { steps: 8 })
  await page.mouse.up()
  const moved = await panel.boundingBox()
  assert.ok(moved.x < start.x - 30, 'Preview moves inside the app')
  const body = await page.locator('.desktop-body').boundingBox()
  await page.mouse.move(moved.x + 60, moved.y + 20)
  await page.mouse.down()
  await page.mouse.move(-100, -100, { steps: 8 })
  await page.mouse.up()
  const clamped = await panel.boundingBox()
  assert.ok(clamped.x >= body.x && clamped.y >= body.y, 'Drag remains in content bounds')
  await panel.hover()
  await panel.getByRole('button', { name: '收起操作预览' }).click()
  await expect(panel.getByRole('img', { name: /的实时操作画面/ })).toHaveCount(0)
  assert.ok((await panel.boundingBox()).height < 48)
  const restore = panel.getByRole('button', { name: '展开操作预览' })
  await restore.hover()
  const smallStart = await restore.boundingBox()
  await page.mouse.move(smallStart.x + 18, smallStart.y + 18)
  await page.mouse.down()
  await page.mouse.move(smallStart.x + 158, smallStart.y + 118, { steps: 8 })
  await page.mouse.up()
  await expect(restore).toBeVisible()
  await expect(panel.getByRole('img', { name: /的实时操作画面/ })).toHaveCount(0)
  const smallMoved = await panel.boundingBox()
  assert.ok(
    smallMoved.x > smallStart.x + 100 && smallMoved.y > smallStart.y + 70,
    `Collapsed preview can be dragged without expanding: ${JSON.stringify({ smallStart, smallMoved })}`,
  )
  await restore.focus()
  await page.keyboard.press('ArrowRight')
  await expect.poll(async () => Math.round((await panel.boundingBox()).x - smallMoved.x)).toBe(16)
  const smallPosition = await panel.boundingBox()
  await page.screenshot({ path: resolve('test-results/computer-preview-collapsed.png') })
  await restore.click()
  await expect(panel.getByRole('img', { name: /的实时操作画面/ })).toBeVisible()
  const expanded = await panel.boundingBox()
  assert.equal(
    Math.round(expanded.x),
    Math.round(smallPosition.x),
    'Expansion keeps the dragged position',
  )
  assert.equal(Math.round(expanded.y), Math.round(smallPosition.y))
  await panel.hover()
  await panel.getByRole('button', { name: '收起操作预览' }).click()
  // Small pointer jitter should still act as a click. Enter also remains usable.
  const small = await restore.boundingBox()
  await page.mouse.move(small.x + 18, small.y + 18)
  await page.mouse.down()
  await page.mouse.move(small.x + 19, small.y + 19)
  await page.mouse.up()
  await expect(frame).toBeVisible()
  await panel.hover()
  await panel.getByRole('button', { name: '收起操作预览' }).click()
  await restore.focus()
  await page.keyboard.press('Enter')
  await expect(frame).toBeVisible()
  await surface.focus()
  await page.keyboard.press('ArrowRight')
  assert.ok((await panel.boundingBox()).x > clamped.x, 'Keyboard can move the view')
  await emit({ ...source, id: 'second', title: '协作任务应用' })
  await emit({ ...source, id: 'third', title: '第三个窗口' })
  await emit({ ...source, id: 'fourth', title: '第四个窗口' })
  await expect(panel.locator('.computer-preview-back')).toHaveCount(2)
  // The host strips frames while a rear source is paused. Its metadata must
  // not erase the thumbnail already received by the renderer.
  const rear = panel.getByRole('button', { name: '查看 协作任务应用 的预览' })
  const rearImage = rear.locator('img')
  await emit({
    ...source,
    id: 'second',
    title: '协作任务应用',
    jpeg: undefined,
    pointer: undefined,
  })
  await expect(rearImage).toHaveAttribute('src', `data:image/jpeg;base64,${jpeg}`)
  await expect(panel.locator('select')).toHaveCount(0)
  await expect(panel.getByText('测试应用', { exact: true })).toHaveCount(0)
  await expect(panel.getByRole('img', { name: /的实时操作画面/ })).toHaveAttribute(
    'alt',
    '测试应用 的实时操作画面',
  )
  await expect
    .poll(async () => {
      const stack = await panel.boundingBox()
      const front = await surface.boundingBox()
      return front.y > stack.y + 25
    })
    .toBe(true)
  // Click the exposed edge, not the hidden center of the rear card.
  await panel
    .getByRole('button', { name: '查看 协作任务应用 的预览' })
    .click({ position: { x: 80, y: 7 } })
  await expect(panel.getByRole('img', { name: /的实时操作画面/ })).toHaveAttribute(
    'alt',
    '协作任务应用 的实时操作画面',
  )
  await emit({ ...source, title: '原窗口更新' })
  await expect(panel.getByRole('img', { name: /的实时操作画面/ })).toHaveAttribute(
    'alt',
    '协作任务应用 的实时操作画面',
  )
  await panel.getByRole('button', { name: /切换预览/ }).click()
  await panel.getByRole('button', { name: /切换预览/ }).click()
  await expect(panel.getByRole('img', { name: /的实时操作画面/ })).toHaveAttribute(
    'alt',
    '第四个窗口 的实时操作画面',
  )
  await emit({ id: 'fourth', active: false })
  await expect(panel.getByRole('img', { name: /的实时操作画面/ })).toHaveAttribute(
    'alt',
    '原窗口更新 的实时操作画面',
  )
  // Whole stack, including its rear cards, stays inside the app when resized.
  await application.evaluate(({ BrowserWindow }) =>
    BrowserWindow.getAllWindows()[0].setSize(1000, 700),
  )
  const bounds = await page.locator('.desktop-body').boundingBox()
  await expect
    .poll(async () => {
      const rect = await panel.boundingBox()
      return (
        rect.x >= bounds.x &&
        rect.y >= bounds.y &&
        rect.x + rect.width <= bounds.x + bounds.width &&
        rect.y + rect.height <= bounds.y + bounds.height
      )
    })
    .toBe(true)
  await page.mouse.click(700, 100)
  await expect(panel.getByRole('button', { name: '收起操作预览' })).toHaveCSS('opacity', '0')
  await page.screenshot({ path: resolve('test-results/computer-preview-stacked.png') })
  await panel.screenshot({ path: resolve('test-results/computer-preview-stacked-card.png') })
  await emit({ id: 'second', active: false })
  await emit({ id: 'third', active: false })
  await expect(panel.locator('.computer-preview-back')).toHaveCount(0)
  const single = await panel.boundingBox()
  assert.equal(
    Math.round(single.height),
    Math.round((await surface.boundingBox()).height),
    'No title bar above the preview',
  )
  await emit({ id: 'fixture', active: false })
  await expect(panel).toHaveCount(0)
  assert.deepEqual(errors, [])
  console.log(
    'Passed preview repaint/restore, collapsed dragging and activation, title-free stack, source switching, drag bounds and cleanup',
  )
} catch (error) {
  const page = application?.windows()[0]
  await page?.screenshot({ path: resolve('test-results/computer-preview-failure.png') }).catch(() => {})
  throw error
} finally {
  await application?.close()
  await rm(data, { recursive: true, force: true })
}
