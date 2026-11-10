class Service {
	constructor(
		public name: string,
		private readonly port: number,
		protected enabled = true,
	) {}

	describe(): string {
		return `${this.name}:${this.port}:${this.enabled}`;
	}
}

export { Service };
