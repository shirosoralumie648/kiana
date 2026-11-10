const url: string = import.meta.url;
module.exports = function metaFactory(): string {
	return url;
};
