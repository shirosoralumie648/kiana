const events = require("node:events");
module.exports = {
	same: events === require("events"),
	hasEventEmitter: typeof events.EventEmitter,
};
