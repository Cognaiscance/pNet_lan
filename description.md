# pNet LAN description
The pNet LAN will consist of a server part and a client part (much like other pNet projects). The server part runs on the primary server grade of a pNet network. The client part runs on a device grade of the pNet netowrk.

## server

###  web interface
* Lists all user devices. For each contact lists all devices.
* each listed device can be assigned an ip address (or none)

### actions
* stores the ip address to device assignment.
* pushes and retries sending updates to online user devices when the ip assignments when changes occur
	* this keeps the ip address assignments up to date
	* we will need to store for each dg with the client app installed the recieved_at timestamp
	* store an updated_at timestamp for the ip address assignment list as a whole
* responds to a request from a client device (of the users devices, not the devices of contacts devices) to get the ip address assignment. Confirms arrival of the ip address assignment and updates the recieved_at timestamp.

## client
* on startup requests an update of the assignment list
* recieves push updates from the server of assignment list changes
* connects to / reserves the ip addresses in the assignment list
	* this client's own address is the one assigned to this device. The other addresses are routes to other devices.
	* traffic sent to one of these addresses is forwarded through pNet to the device this user assigned that address to.
	* traffic recieved through pNet is sent to this device as if coming from the address this user assigned to the sending device, and to the address this user assigned to this device.

## addresses are private labels
Each user assigns whatever addresses they want for their own devices and for their contacts' devices. The addresses do not have to match. William can call Chad's laptop 10.0.0.4 while Chad calls that same laptop 192.168.50.2, and calls William's laptop something else again. The pNet device is the identity. The IP address is only how that device appears on this user's machines.

Within one user's own devices the list is shared, so those addresses match. Across a contact the two lists are allowed to disagree.

When the client takes a packet off the local interface for an address in its list, it strips the addresses and sends the packet through pNet to the device that address names. The receiving client sees the packet as for this device, from that sending device. It does not see the sender's address numbers. It writes the addresses from its own assignment list: the source is the address it assigned to the sender, and the destination is the address it assigned to itself. A reply uses the address on the machine that is replying, and the other side rewrites it again from its own list.

Both sides still need an address for the other device, or there is nothing to write on the packet. TCP and UDP follow this. A protocol that writes an IP address inside the payload does not.
