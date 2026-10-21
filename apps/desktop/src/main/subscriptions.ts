export class SubscriptionRegistry {
  private subscriptions = new Map<string, AbortController>()

  start(owner: number, token: string): AbortController {
    this.stop(owner, token)
    const controller = new AbortController()
    this.subscriptions.set(`${owner}:${token}`, controller)
    return controller
  }

  stop(owner: number, token: string): void {
    const key = `${owner}:${token}`
    this.subscriptions.get(key)?.abort()
    this.subscriptions.delete(key)
  }

  stopOwner(owner: number): void {
    for (const [key, controller] of this.subscriptions) {
      if (key.startsWith(`${owner}:`)) {
        controller.abort()
        this.subscriptions.delete(key)
      }
    }
  }

  stopAll(): void {
    for (const controller of this.subscriptions.values()) controller.abort()
    this.subscriptions.clear()
  }
}
