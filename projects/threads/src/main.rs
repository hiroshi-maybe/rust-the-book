use std::thread;
use std::time::Duration;
use std::sync::mpsc::{self, Sender};

fn main() {
    {
        // Creating a New Thread with spawn

        let handle = thread::spawn(|| {
            for i in 1..10 {
                println!("hi number {} from the spawned thread!", i);
                thread::sleep(Duration::from_millis(1));
            }
        });

        for i in 1..5 {
            println!("hi number {} from the main thread!", i);
            thread::sleep(Duration::from_millis(1));
        }

        handle.join().unwrap();

        println!("The spawned thread joined");
    }

    {
        // Using move Closures with Threads

        let v = vec![1, 2, 3];
        let handle = thread::spawn(move || {
            println!("Here's a moved vector in a new thread: {:?}", v);
        });

        // Throws an error because `v` is moved above
        // println!("{:?}", v);

        handle.join().unwrap();
    }

    {
        // Using Message Passing to Transfer Data Between Threads

        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            let val = String::from("hi");
            tx.send(val).unwrap();
            // Error: `borrow of moved value: `val``
            // println!("val is {}", val);
        });

        let received = rx.recv().unwrap();
        println!("Got: {}", received);
    }

    {
        // Creating Multiple Producers by Cloning the Transmitter

        let (tx, rx) = mpsc::channel();

        fn send(tx: Sender<String>, vals: Vec<String>) {
            thread::spawn(move || {
                for val in vals {
                    tx.send(val).unwrap();
                    thread::sleep(Duration::from_secs(1));
                }
            });
        }

        {
            let vals = vec![
                String::from("hi"),
                String::from("from"),
                String::from("the"),
                String::from("thread"),
            ];
            let tx1 = Sender::clone(&tx);
            send(tx1, vals);
        }

        {
            let vals = vec![
                String::from("more"),
                String::from("messages"),
                String::from("for"),
                String::from("you"),
            ];
            send(tx, vals);
        }

        for received in rx {
            println!("Got: {}", received);
        }
    }
}
