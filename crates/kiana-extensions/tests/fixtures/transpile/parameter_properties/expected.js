class Service {
	name;
	port;
	enabled;
	constructor(name, port, enabled = true) {
		this.name = name;
		this.port = port;
		this.enabled = enabled;
	}
	describe() {
		return `${this.name}:${this.port}:${this.enabled}`;
	}
}
export { Service };
