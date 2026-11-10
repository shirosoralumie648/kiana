'use strict';
module.exports = function quote(xs) {
	return xs.map(function (s) { return "'" + s + "'"; }).join(" ");
};
