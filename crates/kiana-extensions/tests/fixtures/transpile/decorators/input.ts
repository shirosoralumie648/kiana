function sealed(target: Function): void {}
function log(target: unknown, key: string, descriptor: PropertyDescriptor): void {}

@sealed
class Widget {
	@log
	method(): string {
		return "ok";
	}
}

export { Widget };
